//! Classed point, circle and simple polygon annotations in a frozen local frame.
use super::*;

pub const SHAPE_RECORD: &str = "_rustgis_shape";
pub const CIRCLE_SEGMENTS: usize = 256;
pub const MAX_VERTICES: usize = 512;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum Shape {
    Point { position: [f64; 2] },
    Circle { center: [f64; 2], radius: f64 },
    Polygon { vertices: Vec<[f64; 2]> },
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShapeSpec {
    pub version: u32,
    pub frame: Frame,
    pub shape: Shape,
}
impl ShapeSpec {
    pub fn tool(&self) -> crate::Tool {
        match self.shape {
            Shape::Point { .. } => crate::Tool::AnnotationPoint,
            Shape::Circle { .. } => crate::Tool::AnnotationCircle,
            Shape::Polygon { .. } => crate::Tool::AnnotationPolygon,
        }
    }
    /// Points remain points. Circles retain exact parameters and export a 256-edge polygon.
    pub fn geometry(&self) -> Result<Geometry, String> {
        if self.version != 1 {
            return Err("unsupported annotation metadata version".into());
        }
        if !self.frame.valid() || self.frame.crs.lacks_datum_shift() || self.frame.transformer().is_none() {
            return Err("annotation needs a supported coordinate reference frame".into());
        }
        let convert = |p: [f64; 2]| -> Result<Coord, String> {
            if !p.iter().all(|v| v.is_finite() && v.abs() <= 1e10) {
                return Err("annotation coordinates must be finite and within the local editing range".into());
            }
            self.frame
                .to_lonlat(p)
                .filter(|c| c.x.is_finite() && c.y.is_finite() && c.x.abs() <= 180. && c.y.abs() < 85.05)
                .ok_or_else(|| "annotation crosses an unsupported projection boundary or polar editing limit".into())
        };
        let local = match &self.shape {
            Shape::Point { position } => return Ok(Geometry::Point(convert(*position)?)),
            Shape::Circle { center, radius } => {
                if !radius.is_finite() || *radius < 1e-6 || *radius > 1e10 {
                    return Err("circle radius must be finite and positive".into());
                }
                (0..CIRCLE_SEGMENTS)
                    .map(|i| {
                        let a = std::f64::consts::TAU * i as f64 / CIRCLE_SEGMENTS as f64;
                        [center[0] + radius * a.cos(), center[1] + radius * a.sin()]
                    })
                    .collect::<Vec<_>>()
            }
            Shape::Polygon { vertices } => {
                if !(3..=MAX_VERTICES).contains(&vertices.len()) {
                    return Err(format!("a polygon needs 3 to {MAX_VERTICES} distinct vertices"));
                }
                vertices.clone()
            }
        };
        if matches!(self.shape, Shape::Polygon { .. }) {
            let grid = local.iter().map(|p| Coord::new(p[0], p[1])).collect::<Vec<_>>();
            if grid.iter().any(|p| !p.x.is_finite() || !p.y.is_finite() || p.x.abs() > 1e10 || p.y.abs() > 1e10) || ring_area(&grid) == 0. {
                return Err("polygon needs finite coordinates and nonzero area in its reference grid".into());
            }
            validate_ring(&grid)?;
        }
        let mut ring = local.into_iter().map(convert).collect::<Result<Vec<_>, _>>()?;
        if ring.iter().map(|c| c.x).fold(f64::NEG_INFINITY, f64::max) - ring.iter().map(|c| c.x).fold(f64::INFINITY, f64::min) > 180. {
            return Err("annotations crossing the antimeridian are not supported".into());
        }
        validate_ring(&ring)?;
        let area = ring_area(&ring);
        if !area.is_finite() || area == 0. {
            return Err("polygon has zero area".into());
        }
        if area < 0. {
            ring.reverse();
        }
        ring.push(*ring.first().ok_or("missing polygon vertex")?);
        Ok(Geometry::Polygon(vec![vec![ring]]))
    }
    /// Never silently refit externally edited geometry to an old construction record.
    pub fn from_row(ds: &Dataset, row: usize) -> Result<Self, String> {
        let raw = ds
            .field_index(SHAPE_RECORD)
            .and_then(|i| match ds.value(row, i) {
                Cell::Text(t) => Some(t),
                _ => None,
            })
            .ok_or("this feature has no annotation shape record")?;
        if raw.len() > 131_072 {
            return Err("annotation record is too large".into());
        }
        let shape: Self = serde_json::from_str(raw).map_err(|_| "invalid annotation shape record")?;
        let want = shape.geometry()?;
        let actual = ds.features.get(row).and_then(|f| f.geometry.as_ref()).ok_or("annotation geometry is missing")?;
        if !same_geometry(actual, &want) {
            return Err("geometry differs from its annotation record; use ordinary feature editing".into());
        }
        Ok(shape)
    }
}
fn ring_area(r: &[Coord]) -> f64 {
    let Some(o) = r.first() else {
        return 0.;
    };
    r.iter().zip(r.iter().cycle().skip(1)).take(r.len()).map(|(a, b)| (a.x - o.x) * (b.y - o.y) - (b.x - o.x) * (a.y - o.y)).sum()
}
fn cross(a: Coord, b: Coord, c: Coord) -> f64 {
    (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)
}
fn between(a: Coord, b: Coord, p: Coord) -> bool {
    cross(a, b, p) == 0. && p.x >= a.x.min(b.x) && p.x <= a.x.max(b.x) && p.y >= a.y.min(b.y) && p.y <= a.y.max(b.y)
}
fn validate_ring(r: &[Coord]) -> Result<(), String> {
    for i in 0..r.len() {
        let (Some(&a), Some(&b)) = (r.get(i), r.get((i + 1) % r.len())) else { return Err("missing polygon edge".into()) };
        if a == b {
            return Err("polygon has a repeated adjacent vertex".into());
        }
        for j in i + 1..r.len() {
            if j == i + 1 || (i == 0 && j == r.len() - 1) {
                continue;
            }
            let (Some(&c), Some(&d)) = (r.get(j), r.get((j + 1) % r.len())) else { return Err("missing polygon edge".into()) };
            let (ac, ad, ca, cb) = (cross(a, b, c), cross(a, b, d), cross(c, d, a), cross(c, d, b));
            if (ac.signum() != ad.signum() && ca.signum() != cb.signum() && ac != 0. && ad != 0. && ca != 0. && cb != 0.)
                || between(a, b, c)
                || between(a, b, d)
                || between(c, d, a)
                || between(c, d, b)
            {
                return Err("polygon edges intersect or touch; draw a simple outline".into());
            }
        }
    }
    Ok(())
}
fn same_geometry(a: &Geometry, b: &Geometry) -> bool {
    let close = |a: &Coord, b: &Coord| (a.x - b.x).abs() < 1e-9 && (a.y - b.y).abs() < 1e-9;
    match (a, b) {
        (Geometry::Point(a), Geometry::Point(b)) => close(a, b),
        (Geometry::Polygon(a), Geometry::Polygon(b)) if a.len() == 1 && b.len() == 1 => {
            let (Some(ap), Some(bp)) = (a.first(), b.first()) else { return false };
            if ap.len() != 1 || bp.len() != 1 {
                return false;
            }
            let (Some(a), Some(b)) = (ap.first(), bp.first()) else { return false };
            let n = b.len().saturating_sub(1);
            n >= 3
                && a.len() == b.len()
                && a.first().zip(a.last()).is_some_and(|(a, b)| close(a, b))
                && (0..n).any(|offset| {
                    [false, true].into_iter().any(|rev| {
                        (0..n).all(|i| {
                            let at = if rev { (offset + n - i) % n } else { (offset + i) % n };
                            a.get(i).zip(b.get(at)).is_some_and(|(a, b)| close(a, b))
                        })
                    })
                })
        }
        _ => false,
    }
}
fn parsed(p: &Value) -> Result<ShapeSpec, String> {
    let value = p.get("shape").ok_or("shape is required")?;
    if value.to_string().len() > 131_072 {
        return Err("annotation record is too large".into());
    }
    let shape: ShapeSpec = serde_json::from_value(value.clone()).map_err(|e| format!("invalid annotation shape: {e}"))?;
    shape.geometry()?;
    Ok(shape)
}
pub(super) fn activate(s: &mut Session, p: &Value) -> CmdResult {
    let tool = match ps(p, "shape").ok_or("shape is required")? {
        "box" => crate::Tool::RotatedBox,
        "circle" => crate::Tool::AnnotationCircle,
        "polygon" => crate::Tool::AnnotationPolygon,
        "point" => crate::Tool::AnnotationPoint,
        _ => return Err("shape must be box, circle, polygon or point".into()),
    };
    let id = s.layer_id(p)?;
    check_layer(s, id)?;
    let l = s.map().layer(id).ok_or("no layer")?;
    if !s.map().draws_at(l, s.scale()) {
        return Err("show the annotation layer and its groups at this scale before annotating".into());
    }
    s.active_layer = Some(id);
    s.tool = tool;
    Ok(json!({"tool":tool.code()}))
}
pub(super) fn add(s: &mut Session, p: &Value) -> CmdResult {
    let id = s.layer_id(p)?;
    check_layer(s, id)?;
    let shape = parsed(p)?;
    image::check_frame(s, id, &shape.frame)?;
    let g = shape.geometry()?;
    let name = super::class(p)?.unwrap_or("");
    let converted = super::ensure_class(s.map_mut().layer_mut(id).ok_or("no layer")?, name, None)?;
    let uid = super::unique_id(s, id)?;
    let record = serde_json::to_string(&shape).map_err(|e| e.to_string())?;
    s.map_mut().layer_mut(id).ok_or("no layer")?.dataset.ensure_field(SHAPE_RECORD, FieldType::Text);
    let result = super::insert(
        s,
        id,
        g,
        &[("annotation_id", Cell::Text(uid)), ("class", Cell::Text(name.into())), (RECORD, Cell::Null), (SHAPE_RECORD, Cell::Text(record))],
    )?;
    let row = result.get("row").and_then(Value::as_u64).and_then(|r| usize::try_from(r).ok()).ok_or("annotation was not added")?;
    ShapeSpec::from_row(&s.map().layer(id).ok_or("no layer")?.dataset, row)?;
    notice(s, &shape, id, converted, true);
    Ok(result)
}
pub(super) fn update(s: &mut Session, p: &Value) -> CmdResult {
    let id = s.layer_id(p)?;
    check_layer(s, id)?;
    let row = uint(p, "row").and_then(|r| usize::try_from(r).ok()).ok_or("row is required")?;
    let shape = parsed(p)?;
    image::check_frame(s, id, &shape.frame)?;
    let old = ShapeSpec::from_row(&s.map().layer(id).ok_or("no layer")?.dataset, row)?;
    if old.frame != shape.frame || std::mem::discriminant(&old.shape) != std::mem::discriminant(&shape.shape) {
        return Err("an existing annotation's shape kind and reference frame cannot change".into());
    }
    let name = super::class(p)?;
    let converted = match name {
        Some(n) => super::ensure_class(s.map_mut().layer_mut(id).ok_or("no layer")?, n, None)?,
        None => false,
    };
    let l = s.map_mut().layer_mut(id).ok_or("no layer")?;
    let before = l.dataset.features.get(row).cloned().ok_or("no feature")?;
    let ri = l.dataset.field_index(SHAPE_RECORD).ok_or("missing annotation record field")?;
    let ci = l.dataset.field_index("class").ok_or("missing class field")?;
    let f = l.dataset.features.get_mut(row).ok_or("no feature")?;
    f.geometry = Some(shape.geometry()?);
    *f.attrs.get_mut(ri).ok_or("missing annotation record")? = Cell::Text(serde_json::to_string(&shape).map_err(|e| e.to_string())?);
    if let Some(name) = name {
        *f.attrs.get_mut(ci).ok_or("missing class value")? = Cell::Text(name.into());
    }
    l.touch_geometry();
    super::super::design::apply(s, id, vec![super::super::design::Change { before: Some(before), row: Some(row) }])?;
    ShapeSpec::from_row(&s.map().layer(id).ok_or("no layer")?.dataset, row)?;
    notice(s, &shape, id, converted, old != shape);
    Ok(json!({"row":row}))
}
fn notice(s: &mut Session, shape: &ShapeSpec, id: u32, converted: bool, constructed: bool) {
    if converted {
        s.notify(format!("Layer {id}: 1 layer changed to class-based annotation symbology"), false);
    }
    if constructed && matches!(shape.shape, Shape::Circle { .. }) {
        s.notify(format!("Layer {id}: 1 circle represented by a {CIRCLE_SEGMENTS}-edge polygon; exact centre, radius and frame are retained in {SHAPE_RECORD}."),false);
    }
    if constructed && matches!(shape.shape, Shape::Polygon { .. }) && shape.frame.crs != Crs::LonLat {
        s.notify(format!("Layer {id}: 1 annotation polygon represented by transformed longitude/latitude vertices; curved frame edges are not densified. Local vertices are retained in {SHAPE_RECORD}."), false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn spec(shape: Shape) -> ShapeSpec {
        ShapeSpec { version: 1, frame: Frame::default(), shape }
    }
    fn shapes() -> [ShapeSpec; 3] {
        [
            spec(Shape::Point { position: [100., 100.] }),
            spec(Shape::Circle { center: [300., 100.], radius: 40. }),
            spec(Shape::Polygon { vertices: vec![[0., 0.], [120., 0.], [60., 30.], [0., 80.]] }),
        ]
    }
    #[test]
    fn annotation_shapes_share_classes_layer_and_geojson_roundtrip() {
        let mut s = Session::new(crate::Project::default());
        s.run("annotation.newLayer", &json!({})).unwrap();
        let id = s.active_layer.unwrap();
        let undo = s.undo_label();
        s.run("annotation.tool", &json!({"shape":"circle"})).unwrap();
        assert_eq!(s.undo_label(), undo, "choosing a shape is not a feature edit");
        for shape in shapes() {
            s.run("annotation.addShape", &json!({"shape":shape,"class":"object"})).unwrap();
        }
        let box_spec = BoxSpec { version: 1, frame: Frame::default(), center: [600., 100.], width: 100., height: 40., angle: 35. };
        s.run("annotation.addBox", &json!({"box":box_spec,"class":"object"})).unwrap();
        s.run("annotation.setClass", &json!({"class":"object","color":"#E53444"})).unwrap();
        let l = s.map().layer(id).unwrap();
        assert_eq!(l.dataset.features.len(), 4);
        let ids = (0..4)
            .map(|r| l.dataset.value(r, l.dataset.field_index("annotation_id").unwrap()).to_string())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(ids.len(), 4);
        for row in 0..4 {
            assert_eq!(l.renderer.symbol_for(&l.dataset, row).stroke.hex(), "#E53444");
            assert!(l.renderer.symbol_for(&l.dataset, row).size >= 8., "points must stay visible in a mixed polygon layer");
        }
        let (bytes, loss) = rustgis_data::io::write_bytes(&l.dataset, "geojson").unwrap();
        assert_eq!(loss, 0);
        let (ds, _) = rustgis_data::io::read_bytes("annotations.geojson", &bytes).unwrap();
        for (row, shape) in shapes().into_iter().enumerate() {
            assert_eq!(ShapeSpec::from_row(&ds, row).unwrap(), shape);
        }
        assert_eq!(BoxSpec::from_row(&ds, 3).unwrap(), box_spec);
        let Geometry::Polygon(p) = ds.features[1].geometry.as_ref().unwrap() else { panic!("circle polygon") };
        assert_eq!(p[0][0].len(), CIRCLE_SEGMENTS + 1);
        let project: crate::Project = serde_json::from_str(&serde_json::to_string(&s.project).unwrap()).unwrap();
        let s = Session::new(project);
        assert_eq!(ShapeSpec::from_row(&s.map().layer(id).unwrap().dataset, 1).unwrap(), shapes()[1]);
    }
    #[test]
    fn annotation_shapes_refuse_crossings_and_invalid_parameters() {
        for shape in [
            Shape::Polygon { vertices: vec![[0., 0.], [100., 100.], [0., 100.], [100., 0.]] },
            Shape::Polygon { vertices: vec![[0., 0.], [0., 0.], [100., 100.]] },
            Shape::Polygon { vertices: vec![[0., 0.], [100., 0.], [200., 0.]] },
            Shape::Polygon { vertices: vec![[0., 0.], [1e6, 1e6], [2e6, 2e6]] },
            Shape::Polygon { vertices: vec![[0., 0.]; MAX_VERTICES + 1] },
            Shape::Circle { center: [0., 0.], radius: 0. },
            Shape::Circle { center: [0., 0.], radius: f64::INFINITY },
            Shape::Point { position: [f64::NAN, 0.] },
        ] {
            assert!(spec(shape).geometry().is_err());
        }
        let mut s = shapes()[1].clone();
        s.version = 2;
        assert!(s.geometry().is_err());
        s.version = 1;
        s.frame.crs = Crs::Epsg(65535);
        assert!(s.geometry().is_err());
    }
    #[test]
    fn annotation_shape_failures_roll_back_palette_rows_and_undo() {
        let mut s = Session::new(crate::Project::default());
        s.run("annotation.newLayer", &json!({})).unwrap();
        let id = s.active_layer.unwrap();
        // Ensure the new metadata column exists before installing a calculation rule on it.
        s.map_mut().layer_mut(id).unwrap().dataset.ensure_field(SHAPE_RECORD, FieldType::Text);
        s.run("data.attributeRule", &json!({"name":"rewrite","kind":"calculation","field":SHAPE_RECORD,"expression":"'broken'"})).unwrap();
        let before = serde_json::to_value(&s.project).unwrap();
        let undo = s.undo_label();
        assert!(s.run("annotation.addShape", &json!({"shape":shapes()[1],"class":"rejected"})).is_err());
        assert_eq!(serde_json::to_value(&s.project).unwrap(), before);
        assert_eq!(s.undo_label(), undo);
        s.run("data.attributeRule", &json!({"name":"rewrite","remove":true})).unwrap();
        s.run("annotation.addShape", &json!({"shape":shapes()[1],"class":"accepted"})).unwrap();
        s.run("data.attributeRule", &json!({"name":"rewrite","kind":"calculation","field":SHAPE_RECORD,"expression":"'broken'"})).unwrap();
        let before = serde_json::to_value(&s.project).unwrap();
        assert!(s.run("annotation.updateShape", &json!({"row":0,"shape":shapes()[1],"class":"rejected-update"})).is_err());
        assert_eq!(serde_json::to_value(&s.project).unwrap(), before);
    }
    #[test]
    fn annotation_point_only_export_reopens_and_shapes_report_circle_loss_once() {
        let mut s = Session::new(crate::Project::default());
        s.run("annotation.newLayer", &json!({})).unwrap();
        let id = s.active_layer.unwrap();
        let mark = s.notice_mark();
        s.run("annotation.addShape", &json!({"shape":shapes()[0],"class":"tree"})).unwrap();
        assert!(s.notices_since(mark).is_empty());
        let (bytes, _) = rustgis_data::io::write_bytes(&s.map().layer(id).unwrap().dataset, "geojson").unwrap();
        let (ds, _) = rustgis_data::io::read_bytes("points.geojson", &bytes).unwrap();
        let id = super::super::super::layer::add_dataset(&mut s, ds);
        s.run("annotation.resume", &json!({"layer":id})).unwrap();
        check_layer(&s, id).unwrap();
        let mark = s.notice_mark();
        s.run("annotation.addShape", &json!({"shape":shapes()[1],"class":"tree"})).unwrap();
        assert_eq!(s.notices_since(mark).iter().filter(|n| n.text.contains("256-edge")).count(), 1);
        let original = ShapeSpec::from_row(&s.map().layer(id).unwrap().dataset, 1).unwrap();
        s.run("annotation.updateShape", &json!({"row":1,"shape":original,"class":"canopy"})).unwrap();
        s.run("edit.undo", &json!({})).unwrap();
        assert_eq!(ShapeSpec::from_row(&s.map().layer(id).unwrap().dataset, 1).unwrap(), original);
        let l = s.map_mut().layer_mut(id).unwrap();
        let i = l.dataset.field_index(SHAPE_RECORD).unwrap();
        l.dataset.fields[i].ty = FieldType::Integer;
        assert!(check_layer(&s, id).is_err(), "never write construction text into an incompatible imported field");
    }
}
