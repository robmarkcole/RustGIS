//! Constrained boxes stored as ordinary polygon features. The versioned construction record
//! is a text attribute, preserved by existing project and GeoJSON readers and writers.
use super::{ps, uint};
use crate::{CmdResult, CommandSpec as C, Session};
use rustgis_data::raster::{Crs, GeoTransform};
use rustgis_data::{Dataset, Field, FieldType, Geometry, GeometryType, Value as Cell};
use rustgis_geom::Coord;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const RECORD: &str = "_rustgis_box";

/// Frozen annotation coordinates: x right, y down. An image uses its full-resolution grid;
/// without an image, this is the Web Mercator grid (not ground metres).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Frame {
    pub crs: Crs,
    pub transform: GeoTransform,
    pub image: Option<String>,
}
impl Default for Frame {
    fn default() -> Self {
        Self {
            crs: Crs::WebMercator,
            transform: GeoTransform { origin_x: 0., origin_y: 0., pixel_width: 1., pixel_height: -1. },
            image: None,
        }
    }
}
impl Frame {
    pub fn from_reference(s: &Session, id: u32) -> Result<Self, String> {
        let l = s.map().layer(id).ok_or("reference raster no longer exists")?;
        let r = l.raster.as_ref().and_then(|r| r.data.as_ref()).ok_or("choose a loaded reference raster")?;
        if !r.georeferenced || !r.transform.is_valid() || r.overview.is_some() {
            return Err("reference raster must be georeferenced and loaded at full resolution (not an overview)".into());
        }
        if r.crs.lacks_datum_shift() {
            return Err("reference raster needs a datum shift that this version cannot apply accurately".into());
        }
        Ok(Self { crs: r.crs, transform: r.transform, image: Some(l.name.clone()) })
    }
    pub fn to_lonlat(&self, p: [f64; 2]) -> Option<Coord> {
        if !self.transform.is_valid() {
            return None;
        }
        self.crs.transformer().ok()?.to_lonlat(self.transform.to_world(p[0], p[1]))
    }
    pub fn to_local(&self, c: Coord) -> Option<[f64; 2]> {
        if !self.transform.is_valid() {
            return None;
        }
        let c = self.crs.transformer().ok()?.from_lonlat(c)?;
        let (x, y) = self.transform.to_pixel(c);
        (x.is_finite() && y.is_finite()).then_some([x, y])
    }
}

/// Clockwise degrees from the frame's positive x axis. Never rotate previous output vertices:
/// regenerate from these parameters, keeping one centre and two orthogonal axes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoxSpec {
    pub version: u32,
    pub frame: Frame,
    pub center: [f64; 2],
    pub width: f64,
    pub height: f64,
    pub angle: f64,
}
impl BoxSpec {
    pub fn axes(&self) -> ([f64; 2], [f64; 2]) {
        let (s, c) = self.angle.to_radians().sin_cos();
        ([c, s], [-s, c])
    }
    pub fn point(&self, x: f64, y: f64) -> [f64; 2] {
        let (u, v) = self.axes();
        [self.center[0] + u[0] * x + v[0] * y, self.center[1] + u[1] * x + v[1] * y]
    }
    pub fn corners(&self) -> [[f64; 2]; 4] {
        let (w, h) = (self.width / 2., self.height / 2.);
        [self.point(-w, -h), self.point(w, -h), self.point(w, h), self.point(-w, h)]
    }
    pub fn geometry(&self) -> Result<Geometry, String> {
        if self.version != 1 {
            return Err("unsupported rotated-box metadata version".into());
        }
        if !self.frame.transform.is_valid()
            || self.frame.crs.lacks_datum_shift()
            || !self.center.iter().chain([&self.width, &self.height, &self.angle]).all(|v| v.is_finite() && v.abs() <= 1e10)
            || self.width < 1e-6
            || self.height < 1e-6
        {
            return Err("a box needs finite coordinates, a supported frame and positive dimensions".into());
        }
        let mut ring = Vec::new();
        for p in self.corners() {
            let c = self
                .frame
                .to_lonlat(p)
                .filter(|c| c.x.is_finite() && c.y.is_finite() && c.x.abs() <= 180. && c.y.abs() < 85.05)
                .ok_or("box crosses an unsupported projection boundary or the polar editing limit")?;
            ring.push(c);
        }
        if ring.iter().map(|c| c.x).fold(f64::NEG_INFINITY, f64::max) - ring.iter().map(|c| c.x).fold(f64::INFINITY, f64::min) > 180. {
            return Err("boxes crossing the antimeridian are not supported".into());
        }
        // Local rectangles must remain strict convex quadrilaterals after transformation.
        // Cross products use coordinate differences, avoiding cancellation far from (0, 0).
        // Refuse folds/collapsed edges rather than export a self-intersecting polygon.
        let turns: Vec<f64> = (0..4)
            .map(|i| {
                let a = ring[i];
                let b = ring[(i + 1) % 4];
                let c = ring[(i + 2) % 4];
                (b.x - a.x) * (c.y - b.y) - (b.y - a.y) * (c.x - b.x)
            })
            .collect();
        if turns.iter().any(|v| !v.is_finite() || *v == 0. || v.signum() != turns[0].signum()) {
            return Err("box folds or collapses after transformation; use a smaller box".into());
        }
        // GeoJSON exterior rings are counterclockwise in longitude/latitude.
        if turns[0] < 0. {
            ring.reverse();
        }
        ring.push(ring[0]);
        Ok(Geometry::Polygon(vec![vec![ring]]))
    }
    /// Recognise only an unchanged box. Foreign vertex edits never silently get refitted.
    pub fn from_row(ds: &Dataset, row: usize) -> Result<Self, String> {
        let raw = ds
            .field_index(RECORD)
            .and_then(|i| match ds.value(row, i) {
                Cell::Text(t) => Some(t.clone()),
                _ => None,
            })
            .ok_or("this feature has no rotated-box construction record")?;
        if raw.len() > 16_384 {
            return Err("rotated-box construction record is too large".into());
        }
        let b: Self = serde_json::from_str(&raw).map_err(|_| "invalid rotated-box construction record")?;
        let expected = b.geometry()?;
        let Some(Geometry::Polygon(polys)) = ds.features.get(row).and_then(|f| f.geometry.as_ref()) else {
            return Err("box geometry is missing".into());
        };
        let Geometry::Polygon(want) = expected else {
            return Err("box geometry is missing".into());
        };
        let ring = polys.first().and_then(|p| p.first()).ok_or("box geometry is missing")?;
        let target = &want[0][0];
        let close = |a: &Coord, b: &Coord| (a.x - b.x).abs() < 1e-9 && (a.y - b.y).abs() < 1e-9;
        let same = polys.len() == 1
            && polys[0].len() == 1
            && ring.len() == 5
            && close(&ring[0], &ring[4])
            && (0..4).any(|offset| {
                (0..4).all(|i| close(&ring[i], &target[(offset + i) % 4])) || (0..4).all(|i| close(&ring[i], &target[(offset + 4 - i) % 4]))
            });
        if !same {
            return Err("polygon differs from its box record; use Edit Vertices as an ordinary polygon".into());
        }
        Ok(b)
    }
}

pub fn commands() -> Vec<C> {
    vec![
        C::new("annotation.newLayer","New Box Annotation Layer","Edit › Features",new_layer).params(r#"{"name"?: text}"#),
        C::new("annotation.addBox","Add Rotated Box","Edit › Features",add).when(super::design::need_editable)
            .params(r#"{"layer"?, "box": {"version":1,"frame":{"crs":CRS,"transform":GeoTransform,"image":text|null},"center":[x,y],"width":number,"height":number,"angle":clockwise degrees}, "class"?:text}"#),
        C::new("annotation.updateBox","Update Rotated Box","Edit › Features",update).when(super::design::need_editable)
            .params(r#"{"layer"?,"row":number,"box": as annotation.addBox,"class"?:text}"#),
    ]
}
fn new_layer(s: &mut Session, p: &Value) -> CmdResult {
    let name = ps(p, "name").unwrap_or("Annotations").trim();
    if name.is_empty() || name.chars().count() > 200 {
        return Err("layer name must have 1 to 200 characters".into());
    }
    let ds = Dataset::new(
        name,
        GeometryType::Polygon,
        vec![Field::new("annotation_id", FieldType::Text), Field::new("class", FieldType::Text), Field::new(RECORD, FieldType::Text)],
    );
    let id = super::layer::add_dataset(s, ds);
    s.map_mut().editing.settings.enabled = true;
    if let Some(l) = s.map_mut().layer_mut(id) {
        l.renderer.map_symbols(|symbol| {
            symbol.fill = rustgis_map::Rgba::new(59, 130, 246, 35);
            symbol.stroke = rustgis_map::Rgba::rgb(59, 130, 246);
        });
    }
    s.tool = crate::Tool::RotatedBox;
    Ok(json!({"layer":id}))
}
fn parsed(p: &Value) -> Result<BoxSpec, String> {
    let value = p.get("box").ok_or("box is required")?;
    if value.to_string().len() > 16_384 {
        return Err("rotated-box construction record is too large".into());
    }
    let mut b: BoxSpec = serde_json::from_value(value.clone()).map_err(|e| format!("invalid box: {e}"))?;
    b.geometry()?;
    b.angle = b.angle.rem_euclid(360.);
    Ok(b)
}
fn check_layer(s: &Session, id: u32) -> Result<(), String> {
    let l = s.map().layer(id).ok_or("no layer")?;
    if l.is_streamed()
        || l.graphics
        || l.geometry_type() != GeometryType::Polygon
        || ["annotation_id", "class", RECORD]
            .iter()
            .any(|n| l.dataset.field_index(n).and_then(|i| l.dataset.fields.get(i)).is_none_or(|f| f.ty != FieldType::Text))
    {
        return Err("select a box annotation layer (New Box Annotation Layer)".into());
    }
    Ok(())
}
fn class(p: &Value) -> Result<Option<&str>, String> {
    match p.get("class") {
        None => Ok(None),
        Some(Value::String(c)) if c.chars().count() <= 200 => Ok(Some(c)),
        _ => Err("class must be text of at most 200 characters".into()),
    }
}
fn add(s: &mut Session, p: &Value) -> CmdResult {
    let id = s.layer_id(p)?;
    check_layer(s, id)?;
    let b = parsed(p)?;
    let g = b.geometry()?;
    let record = serde_json::to_string(&b).map_err(|e| e.to_string())?;
    // Feature row IDs can be reassigned by GeoJSON readers. Keep a separate ordinary
    // property so annotation identity survives exports, filtering and subsequent imports.
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let tick = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|t| t.as_nanos()).unwrap_or(0);
    let ds = &s.map().layer(id).ok_or("no layer")?.dataset;
    let fi = ds.field_index("annotation_id").ok_or("missing annotation_id field")?;
    let uid = loop {
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let uid = format!("box-{tick:x}-{n:x}");
        if !ds.features.iter().any(|f| f.attrs.get(fi) == Some(&Cell::Text(uid.clone()))) {
            break uid;
        }
    };
    let result = super::edit::add_feature(
        s,
        &json!({"layer":id,"geometry":rustgis_data::io::geometry_to_geojson(&g),"attributes":{"annotation_id":uid,"class":class(p)?.unwrap_or(""),RECORD:record}}),
    )?;
    let row = result.get("row").and_then(Value::as_u64).and_then(|r| usize::try_from(r).ok()).ok_or("box was not added")?;
    BoxSpec::from_row(&s.map().layer(id).ok_or("no layer")?.dataset, row)?;
    Ok(result)
}
fn update(s: &mut Session, p: &Value) -> CmdResult {
    let id = s.layer_id(p)?;
    check_layer(s, id)?;
    let row = uint(p, "row").and_then(|r| usize::try_from(r).ok()).ok_or("row is required")?;
    let b = parsed(p)?;
    let g = b.geometry()?;
    let c = class(p)?;
    let l = s.map().layer(id).ok_or("no layer")?;
    let old = BoxSpec::from_row(&l.dataset, row)?;
    if b.frame != old.frame {
        return Err("an existing box's reference frame cannot change".into());
    }
    let record = serde_json::to_string(&b).map_err(|e| e.to_string())?;
    let l = s.map_mut().layer_mut(id).ok_or("no layer")?;
    let before = l.dataset.features.get(row).cloned().ok_or("no feature")?;
    let ri = l.dataset.field_index(RECORD).ok_or("no construction field")?;
    let ci = l.dataset.field_index("class").ok_or("no class field")?;
    let feature = l.dataset.features.get_mut(row).ok_or("no feature")?;
    feature.geometry = Some(g);
    *feature.attrs.get_mut(ri).ok_or("missing construction value")? = Cell::Text(record);
    if let Some(c) = c {
        *feature.attrs.get_mut(ci).ok_or("missing class value")? = Cell::Text(c.into());
    }
    l.touch_geometry();
    super::design::apply(s, id, vec![super::design::Change { before: Some(before), row: Some(row) }])?;
    BoxSpec::from_row(&s.map().layer(id).ok_or("no layer")?.dataset, row)?;
    Ok(json!({"row":row}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Project;
    fn spec() -> BoxSpec {
        BoxSpec { version: 1, frame: Frame::default(), center: [1000., -2000.], width: 120., height: 30., angle: 37. }
    }
    #[test]
    fn annotation_commands_roundtrip_and_undo_without_refitting() {
        let mut s = Session::new(Project::default());
        s.run("annotation.newLayer", &json!({})).unwrap();
        let id = s.active_layer.unwrap();
        let b = spec();
        s.run("annotation.addBox", &json!({"box":b,"class":"vehicle"})).unwrap();
        let read = |s: &Session| BoxSpec::from_row(&s.map().layer(id).unwrap().dataset, 0).unwrap();
        assert_eq!(read(&s), b);
        let feature_id = s.map().layer(id).unwrap().dataset.features[0].id;
        let mut changed = b.clone();
        changed.angle = 721.25;
        changed.width = 90.;
        s.run("annotation.updateBox", &json!({"row":0,"box":changed})).unwrap();
        assert_eq!(read(&s).angle, 1.25);
        assert!(s.undo());
        assert_eq!(read(&s), b);
        assert!(s.redo());
        assert_eq!(read(&s).width, 90.);
        let ds = &s.map().layer(id).unwrap().dataset;
        assert_eq!(ds.features[0].id, feature_id);
        let annotation_id = ds.value(0, ds.field_index("annotation_id").unwrap()).clone();
        let (bytes, loss) = rustgis_data::io::write_bytes(ds, "geojson").unwrap();
        assert_eq!(loss, 0);
        let (ds, report) = rustgis_data::io::read_bytes("boxes.geojson", &bytes).unwrap();
        assert_eq!(BoxSpec::from_row(&ds, 0).unwrap(), read(&s));
        assert_eq!(ds.value(0, ds.field_index("annotation_id").unwrap()), &annotation_id);
        assert_eq!(ds.value(0, ds.field_index("class").unwrap()), &Cell::Text("vehicle".into()));
        let _ = report;
        let text = serde_json::to_string(&s.project).unwrap();
        let project = Project::from_json(&text).unwrap();
        assert_eq!(BoxSpec::from_row(&project.maps[0].layer(id).unwrap().dataset, 0).unwrap(), read(&s));
        let before = serde_json::to_string(&s.project).unwrap();
        changed.version = 2;
        assert!(s.run("annotation.updateBox", &json!({"row":0,"box":changed})).is_err());
        assert_eq!(serde_json::to_string(&s.project).unwrap(), before);
        let mut ds = ds;
        if let Some(Geometry::Polygon(polys)) = &mut ds.features.get_mut(0).unwrap().geometry {
            polys[0][0][0].x += 0.01;
        }
        assert!(BoxSpec::from_row(&ds, 0).unwrap_err().contains("differs"));
    }
    #[test]
    fn annotation_geometry_is_closed_ccw_and_invalid_inputs_are_refused() {
        for angle in [-720., 0., 45., 90., 179.999, 359.999, 720.] {
            let mut b = spec();
            b.angle = angle;
            let Geometry::Polygon(polys) = b.geometry().unwrap() else { panic!() };
            let r = &polys[0][0];
            assert_eq!(r.len(), 5);
            assert_eq!(r.first(), r.last());
            let signed: f64 = r.windows(2).map(|w| (w[1].x - w[0].x) * (w[1].y + w[0].y)).sum();
            assert!(signed < 0.);
            let c = b.corners();
            let u = [c[1][0] - c[0][0], c[1][1] - c[0][1]];
            let v = [c[2][0] - c[1][0], c[2][1] - c[1][1]];
            assert!((u[0] * v[0] + u[1] * v[1]).abs() < 1e-8);
        }
        for bad in [0., -1., f64::NAN, f64::INFINITY] {
            let mut b = spec();
            b.width = bad;
            assert!(b.geometry().is_err());
        }
        let mut b = spec();
        b.center = [20_037_508., 0.];
        b.width = 1000.;
        assert!(b.geometry().is_err());
        let mut b = spec();
        b.frame.transform.pixel_width = 0.;
        assert!(b.geometry().is_err());
    }
    #[test]
    fn annotation_non_square_pixel_frame_is_exact_in_pixels() {
        let b = BoxSpec {
            frame: Frame {
                crs: Crs::Epsg(32631),
                transform: GeoTransform { origin_x: 500_000., origin_y: 5_400_000., pixel_width: 0.5, pixel_height: -1.5 },
                image: Some("test.tif".into()),
            },
            ..spec()
        };
        for xy in b.corners() {
            let c = b.frame.to_lonlat(xy).unwrap();
            let back = b.frame.to_local(c).unwrap();
            assert!((back[0] - xy[0]).abs() < 1e-5);
            assert!((back[1] - xy[1]).abs() < 1e-5);
        }
        assert!(b.geometry().is_ok());
    }
}
