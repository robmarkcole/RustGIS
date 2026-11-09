//! Constrained boxes stored as ordinary polygon features. The versioned construction record
//! is a text attribute, preserved by existing project and GeoJSON readers and writers.
use super::{ps, uint};
use crate::{CmdResult, CommandSpec as C, Session};
use rustgis_data::raster::{Crs, GeoTransform, Transformer};
use rustgis_data::{Dataset, Field, FieldType, Geometry, GeometryType, Value as Cell};
use rustgis_geom::Coord;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{cell::RefCell, rc::Rc};

pub mod image;
mod shapes;
pub use shapes::{CIRCLE_SEGMENTS, MAX_VERTICES, SHAPE_RECORD, Shape, ShapeSpec};

pub const RECORD: &str = "_rustgis_box";

/// Frozen annotation coordinates: x right, y down. An image uses its full-resolution grid;
/// without an image, this is the Web Mercator grid (not ground metres).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Frame {
    pub crs: Crs,
    pub transform: GeoTransform,
    pub image: Option<String>,
    /// Original TIFF grid for image-bound annotations, including rotation and skew.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub affine: Option<[f64; 6]>,
}
impl Default for Frame {
    fn default() -> Self {
        Self {
            crs: Crs::WebMercator,
            transform: GeoTransform { origin_x: 0., origin_y: 0., pixel_width: 1., pixel_height: -1. },
            image: None,
            affine: None,
        }
    }
}
// A bounded runtime-only cache: repeated EPSG conversions while drawing handles reuse the
// parsed projection. CRS is the entire transformer key; the pixel transform is applied below.
type CachedProjection = Option<(Crs, Rc<Transformer>)>;
thread_local! {
    static PROJECTION: RefCell<CachedProjection> = const { RefCell::new(None) };
}
impl Frame {
    fn transformer(&self) -> Option<Rc<Transformer>> {
        PROJECTION.with(|cache| {
            let mut cache = cache.borrow_mut();
            if let Some((crs, t)) = cache.as_ref()
                && *crs == self.crs
            {
                return Some(Rc::clone(t));
            }
            let t = Rc::new(self.crs.transformer().ok()?);
            *cache = Some((self.crs, Rc::clone(&t)));
            Some(t)
        })
    }
    pub fn from_reference(s: &Session, id: u32) -> Result<Self, String> {
        let l = s.map().layer(id).ok_or("reference raster no longer exists")?;
        let r = l.raster.as_ref().and_then(|r| r.data.as_ref()).ok_or("choose a loaded reference raster")?;
        if !r.georeferenced || !r.transform.is_valid() || r.overview.is_some() {
            return Err("reference raster must be georeferenced and loaded at full resolution (not an overview)".into());
        }
        if r.crs.lacks_datum_shift() {
            return Err("reference raster needs a datum shift that this version cannot apply accurately".into());
        }
        Ok(Self { crs: r.crs, transform: r.transform, image: Some(l.name.clone()), affine: None })
    }
    pub fn valid(&self) -> bool {
        match self.affine {
            Some([x, a, b, y, c, d]) => [x, a, b, y, c, d].iter().all(|v| v.is_finite()) && (a * d - b * c).is_normal(),
            None => self.transform.is_valid(),
        }
    }
    pub fn to_lonlat(&self, p: [f64; 2]) -> Option<Coord> {
        if !self.valid() {
            return None;
        }
        let world = match self.affine {
            Some([x, a, b, y, c, d]) => Coord::new(x + a * p[0] + b * p[1], y + c * p[0] + d * p[1]),
            None => self.transform.to_world(p[0], p[1]),
        };
        self.transformer()?.to_lonlat(world)
    }
    pub fn to_local(&self, c: Coord) -> Option<[f64; 2]> {
        if !self.valid() {
            return None;
        }
        let c = self.transformer()?.from_lonlat(c)?;
        let (x, y) = match self.affine {
            Some([ox, a, b, oy, cx, d]) => {
                let det = a * d - b * cx;
                let (dx, dy) = (c.x - ox, c.y - oy);
                ((d * dx - b * dy) / det, (a * dy - cx * dx) / det)
            }
            None => self.transform.to_pixel(c),
        };
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
        if !self.frame.valid()
            || self.frame.transformer().is_none()
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
        let mut turns = Vec::new();
        for i in 0..4 {
            let a = ring.get(i).ok_or("missing box corner")?;
            let b = ring.get((i + 1) % 4).ok_or("missing box corner")?;
            let c = ring.get((i + 2) % 4).ok_or("missing box corner")?;
            turns.push((b.x - a.x) * (c.y - b.y) - (b.y - a.y) * (c.x - b.x));
        }
        let first_turn = *turns.first().ok_or("missing box corner")?;
        if turns.iter().any(|v| !v.is_finite() || *v == 0. || v.signum() != first_turn.signum()) {
            return Err("box folds or collapses after transformation; use a smaller box".into());
        }
        // GeoJSON exterior rings are counterclockwise in longitude/latitude.
        if first_turn < 0. {
            ring.reverse();
        }
        ring.push(*ring.first().ok_or("missing box corner")?);
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
        let target = want.first().and_then(|p| p.first()).ok_or("box geometry is missing")?;
        let close = |a: &Coord, b: &Coord| (a.x - b.x).abs() < 1e-9 && (a.y - b.y).abs() < 1e-9;
        let same = polys.len() == 1
            && polys.first().is_some_and(|p| p.len() == 1)
            && ring.len() == 5
            && ring.first().zip(ring.get(4)).is_some_and(|(a, b)| close(a, b))
            && (0..4).any(|offset| {
                let matches = |reversed| {
                    (0..4).all(|i| {
                        let at = if reversed { (offset + 4 - i) % 4 } else { (offset + i) % 4 };
                        ring.get(i).zip(target.get(at)).is_some_and(|(a, b)| close(a, b))
                    })
                };
                matches(false) || matches(true)
            });
        if !same {
            return Err("polygon differs from its box record; use Edit Vertices as an ordinary polygon".into());
        }
        Ok(b)
    }
}

// Construction records and class names are user data, including image names that resemble
// addresses. These commands only edit loaded features; new rows can sample elevation below.
const DATA_ROLES: &[crate::ParamRole] = &[
    crate::ParamRole { key: "shape", role: crate::Role::Opaque },
    crate::ParamRole { key: "box", role: crate::Role::Opaque },
    crate::ParamRole { key: "class", role: crate::Role::Opaque },
];

pub fn commands() -> Vec<C> {
    let mut commands = vec![
        C::new("annotation.delete", "Delete Selected Annotation", "Annotate › Edits", delete_annotation)
            .inputs(super::Inputs::single_layer)
            .roles(&[crate::ParamRole {key:"annotation_id",role:crate::Role::Opaque}])
            .effects(crate::policy::Effects::NONE).when(super::design::need_editable)
            .params(r#"{"layer"?:name|id,"annotation_id":text}"#),
        C::new("annotation.tool", "Choose Annotation Shape", "Annotate › Shapes", shapes::activate).roles(&[]).effects(crate::policy::Effects::NONE).view().when(super::design::need_editable)
            .params(r#"{"layer"?:name|id,"shape":"box"|"circle"|"polygon"|"point"}"#),
        C::new("annotation.addShape", "Add Annotation Shape", "Annotate › Shapes", shapes::add).inputs(super::design::new_feature_inputs).roles(DATA_ROLES).effects(crate::policy::Effects::NONE).when(super::design::need_editable)
            .params(r#"{"layer"?:name|id,"shape":{"version":1,"frame":as annotation.addBox,"shape":{"kind":"point","position":[x,y]}|{"kind":"circle","center":[x,y],"radius":number}|{"kind":"polygon","vertices":[[x,y],...] }},"class"?:text}"#),
        C::new("annotation.updateShape", "Update Annotation Shape", "Annotate › Shapes", shapes::update).roles(DATA_ROLES).effects(crate::policy::Effects::NONE).when(super::design::need_editable)
            .params(r#"{"layer"?:name|id,"row":number,"shape":as annotation.addShape,"class"?:text}"#),
        C::new("annotation.newLayer","New Annotation Layer","Annotate › Layers",new_layer).roles(DATA_ROLES).effects(crate::policy::Effects::NONE).params(r#"{"name"?: text}"#),
        C::new("annotation.resume", "Resume Annotation", "Annotate › Shapes", resume).roles(DATA_ROLES).effects(crate::policy::Effects::NONE).params(r#"{"layer"?:name|id}"#),
        C::new("annotation.setClass", "Set Annotation Class", "Annotate › Classes", set_class).roles(DATA_ROLES).effects(crate::policy::Effects::NONE)
            .params(r##"{"layer"?:name|id,"class":text,"color"?:"#RRGGBB"}"##),
        C::new("annotation.addBox","Add Rotated Box","Annotate › Shapes",add).inputs(super::design::new_feature_inputs).roles(DATA_ROLES).effects(crate::policy::Effects::NONE).when(super::design::need_editable)
            .params(r#"{"layer"?, "box": {"version":1,"frame":{"crs":CRS,"transform":GeoTransform,"image":text|null},"center":[x,y],"width":number,"height":number,"angle":clockwise degrees}, "class"?:text}"#),
        C::new("annotation.updateBox","Update Rotated Box","Annotate › Shapes",update).roles(DATA_ROLES).effects(crate::policy::Effects::NONE).when(super::design::need_editable)
            .params(r#"{"layer"?,"row":number,"box": as annotation.addBox,"class"?:text}"#),
    ];
    commands.extend(image::commands());
    commands
}
/// Returns the configured class colour, including classes with no features yet.
pub fn class_colors(s: &Session, id: u32) -> Vec<(String, rustgis_map::Rgba)> {
    match s.map().layer(id).map(|l| &l.renderer) {
        Some(rustgis_map::Renderer::Unique { field, classes, expression: None, .. }) if field.eq_ignore_ascii_case("class") => {
            classes.iter().map(|(name, symbol)| (name.clone(), symbol.stroke)).collect()
        }
        _ => Vec::new(),
    }
}
/// Colour suggested for the next new class; existing class colours remain unchanged.
pub fn default_class_color(i: usize) -> rustgis_map::Rgba {
    use rustgis_map::Rgba;
    let palette = [
        Rgba::rgb(59, 130, 246),
        Rgba::rgb(249, 115, 22),
        Rgba::rgb(34, 197, 94),
        Rgba::rgb(168, 85, 247),
        Rgba::rgb(236, 72, 153),
        Rgba::rgb(6, 182, 212),
        Rgba::rgb(234, 179, 8),
        Rgba::rgb(148, 90, 60),
    ];
    palette.get(i % palette.len()).copied().unwrap_or(Rgba::rgb(59, 130, 246)).shade(1. - (i / palette.len()).min(7) as f64 * 0.08)
}
fn ensure_class(l: &mut rustgis_map::Layer, name: &str, color: Option<rustgis_map::Rgba>) -> Result<bool, String> {
    if l.dataset.annotation.is_some() && name.trim().is_empty() {
        return Err("choose a named class before adding image annotations".into());
    }
    use rustgis_map::symbology::MAX_UNIQUE;
    use rustgis_map::{Ramp, Renderer};
    let converted = !matches!(&l.renderer, Renderer::Unique {field, expression:None, ..} if field.eq_ignore_ascii_case("class"));
    if converted {
        let (renderer, rest) = Renderer::unique_counted(&l.dataset, &[], "class", Ramp::Harbour, l.renderer.base_symbol())?;
        if rest.features > 0 {
            return Err(format!("more than {MAX_UNIQUE} annotation classes; simplify the class list before applying class colours"));
        }
        l.renderer = renderer;
        if let Renderer::Unique { classes, .. } = &mut l.renderer {
            for (_, symbol) in classes {
                symbol.stroke = symbol.fill.with_alpha(255);
                symbol.fill = symbol.fill.with_alpha(45);
                symbol.size = symbol.size.max(8.);
            }
        }
    }
    let Renderer::Unique { classes, other, .. } = &mut l.renderer else { return Err("class renderer is unavailable".into()) };
    let at = match classes.iter().position(|(c, _)| c == name) {
        Some(i) => i,
        None => {
            if classes.len() >= MAX_UNIQUE {
                return Err(format!("at most {MAX_UNIQUE} annotation classes are supported"));
            }
            let i = classes.len();
            let c = default_class_color(i);
            let mut symbol = *other;
            symbol.stroke = c;
            symbol.fill = c.with_alpha(45);
            classes.push((name.to_string(), symbol));
            i
        }
    };
    if let Some((_, symbol)) = classes.get_mut(at) {
        // Polygon base symbols have size zero; mixed annotation layers need visible points.
        symbol.size = symbol.size.max(8.);
        if let Some(c) = color {
            symbol.stroke = c.with_alpha(255);
            symbol.fill = c.with_alpha(45);
        }
    }
    image::sync_palette(l)?;
    Ok(converted)
}
fn set_class(s: &mut Session, p: &Value) -> CmdResult {
    let id = s.layer_id(p)?;
    check_layer(s, id)?;
    let name = class(p)?.ok_or("class is required")?;
    let color = match p.get("color") {
        None => None,
        Some(Value::String(t)) => Some(rustgis_map::Rgba::from_hex(t).ok_or("color must be #RRGGBB or #RRGGBBAA")?),
        Some(_) => return Err("color must be a hex colour string".into()),
    };
    let converted = ensure_class(s.map_mut().layer_mut(id).ok_or("no layer")?, name, color)?;
    if converted {
        s.notify(format!("Layer {id}: 1 layer changed to class-based annotation symbology; features are unchanged"), false);
    }
    Ok(json!({"class":name}))
}
fn resume(s: &mut Session, p: &Value) -> CmdResult {
    let id = s.layer_id(p)?;
    check_layer(s, id)?;
    let l = s.map().layer(id).ok_or("no layer")?;
    if !s.map().draws_at(l, s.scale()) {
        return Err("show the annotation layer and its groups at this scale before annotating".into());
    }
    if !matches!(&l.renderer, rustgis_map::Renderer::Unique {field, expression:None, ..} if field.eq_ignore_ascii_case("class")) {
        let name = l
            .dataset
            .field_index("class")
            .and_then(|fi| match l.dataset.value(0, fi) {
                Cell::Text(t) => Some(t.clone()),
                _ => None,
            })
            .unwrap_or_default();
        if l.dataset.features.is_empty() {
            let l = s.map_mut().layer_mut(id).ok_or("no layer")?;
            l.renderer = rustgis_map::Renderer::unique(&l.dataset, &[], "class", rustgis_map::Ramp::Harbour, l.renderer.base_symbol())?;
        } else {
            ensure_class(s.map_mut().layer_mut(id).ok_or("no layer")?, &name, None)?;
        }
        s.notify(format!("Layer {id}: 1 layer changed to class-based annotation symbology"), false);
    }
    s.active_layer = Some(id);
    s.map_mut().editing.settings.enabled = true;
    s.tool = if s.tool.is_annotation() {
        s.tool
    } else {
        let l = s.map().layer(id).ok_or("no layer")?;
        l.selection.first().and_then(|row| ShapeSpec::from_row(&l.dataset, *row).ok()).map(|s| s.tool()).unwrap_or(crate::Tool::RotatedBox)
    };
    Ok(json!({"layer":id}))
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
        l.renderer = rustgis_map::Renderer::unique(&l.dataset, &[], "class", rustgis_map::Ramp::Harbour, l.renderer.base_symbol())?;
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
/// Validates the shared command and editor schema requirements.
pub fn check_layer(s: &Session, id: u32) -> Result<(), String> {
    let l = s.map().layer(id).ok_or("no layer")?;
    if l.dataset.annotation.as_ref().is_some_and(|m| m.version != 1) {
        return Err("unsupported image annotation metadata version; update RustGIS".into());
    }
    if l.is_streamed()
        || l.graphics
        || !matches!(l.geometry_type(), GeometryType::Polygon | GeometryType::Point)
        || l.dataset.field_index(SHAPE_RECORD).and_then(|i| l.dataset.fields.get(i)).is_some_and(|f| f.ty != FieldType::Text)
        || ["annotation_id", "class", RECORD]
            .iter()
            .any(|n| l.dataset.field_index(n).and_then(|i| l.dataset.fields.get(i)).is_none_or(|f| f.ty != FieldType::Text))
    {
        return Err("select an annotation layer (New Annotation Layer)".into());
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
/// Delete exactly one stable identity, independently of selections in any other layer.
fn delete_annotation(s: &mut Session, p: &Value) -> CmdResult {
    let id = s.layer_id(p)?;
    check_layer(s, id)?;
    let uid =
        ps(p, "annotation_id").filter(|v| !v.is_empty() && v.len() <= 1024).ok_or("annotation_id is required (at most 1024 bytes)")?;
    let l = s.map_mut().layer_mut(id).ok_or("no layer")?;
    let fi = l.dataset.field_index("annotation_id").ok_or("missing annotation_id field")?;
    let mut matches = l
        .dataset
        .features
        .iter()
        .enumerate()
        .filter_map(|(row, f)| matches!(f.attrs.get(fi),Some(Cell::Text(value)) if value==uid).then_some(row));
    let row = matches.next().ok_or("annotation no longer exists")?;
    if matches.next().is_some() {
        return Err("annotation_id is ambiguous; repair duplicate identities before deleting".into());
    }
    let before = l.dataset.features.remove(row);
    l.selection.retain(|selected| *selected != row);
    for selected in &mut l.selection {
        if *selected > row {
            *selected -= 1;
        }
    }
    l.touch_geometry();
    super::design::apply(s, id, vec![super::design::Change { before: Some(before), row: None }])?;
    Ok(json!({"layer":id,"annotation_id":uid,"deleted":1}))
}
fn unique_id(s: &Session, id: u32) -> Result<String, String> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let tick = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|t| t.as_nanos()).unwrap_or(0);
    let ds = &s.map().layer(id).ok_or("no layer")?.dataset;
    let fi = ds.field_index("annotation_id").ok_or("missing annotation_id field")?;
    let uid = loop {
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let uid = format!("box-{tick:x}-{n:x}");
        if !ds.features.iter().any(|f| matches!(f.attrs.get(fi), Some(Cell::Text(t)) if *t == uid)) {
            break uid;
        }
    };
    Ok(uid)
}
fn insert(s: &mut Session, id: u32, geometry: Geometry, given: &[(&str, Cell)]) -> CmdResult {
    let l = s.map_mut().layer_mut(id).ok_or("no layer")?;
    let attrs = l
        .dataset
        .fields
        .iter()
        .map(|f| given.iter().find(|(name, _)| name.eq_ignore_ascii_case(&f.name)).map(|(_, v)| v.clone()).unwrap_or(Cell::Null))
        .collect();
    l.dataset.push(Some(geometry), attrs);
    l.touch_geometry();
    let row = l.dataset.features.len().saturating_sub(1);
    l.selection = vec![row];
    super::design::apply(s, id, vec![super::design::Change { before: None, row: Some(row) }])?;
    Ok(json!({"row":row}))
}
fn add(s: &mut Session, p: &Value) -> CmdResult {
    let id = s.layer_id(p)?;
    check_layer(s, id)?;
    let b = parsed(p)?;
    image::check_frame(s, id, &b.frame)?;
    let name = class(p)?.unwrap_or("");
    let converted = ensure_class(s.map_mut().layer_mut(id).ok_or("no layer")?, name, None)?;
    let g = b.geometry()?;
    let record = serde_json::to_string(&b).map_err(|e| e.to_string())?;
    // Feature row IDs can be reassigned by GeoJSON readers. Keep a separate ordinary
    // property so annotation identity survives exports, filtering and subsequent imports.
    let uid = unique_id(s, id)?;
    let result = insert(s, id, g, &[("annotation_id", Cell::Text(uid)), ("class", Cell::Text(name.into())), (RECORD, Cell::Text(record))])?;
    let row = result.get("row").and_then(Value::as_u64).and_then(|r| usize::try_from(r).ok()).ok_or("box was not added")?;
    BoxSpec::from_row(&s.map().layer(id).ok_or("no layer")?.dataset, row)?;
    if converted {
        s.notify(format!("Layer {id}: 1 layer changed to class-based annotation symbology"), false);
    }
    notify_edges(s, id, row);
    Ok(result)
}
/// Corner transformation is the approximation step; export preserves this ordinary polygon.
fn notify_edges(s: &mut Session, id: u32, row: usize) {
    s.notify(format!("Layer {id}, row {row}: 1 annotation box represented by four transformed longitude/latitude corners; curved frame edges are not densified. Exact local construction is retained in {RECORD}."), false);
}
fn update(s: &mut Session, p: &Value) -> CmdResult {
    let id = s.layer_id(p)?;
    check_layer(s, id)?;
    let row = uint(p, "row").and_then(|r| usize::try_from(r).ok()).ok_or("row is required")?;
    let b = parsed(p)?;
    image::check_frame(s, id, &b.frame)?;
    let g = b.geometry()?;
    let c = class(p)?;
    let converted = match c {
        Some(name) => ensure_class(s.map_mut().layer_mut(id).ok_or("no layer")?, name, None)?,
        None => false,
    };
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
    if converted {
        s.notify(format!("Layer {id}: 1 layer changed to class-based annotation symbology"), false);
    }
    if b != old {
        notify_edges(s, id, row);
    }
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
    fn annotation_commands_keep_loaded_edits_available_to_scoped_callers() {
        use crate::policy::{Call, Effects, Revocation, Scope};
        struct Live;
        impl Revocation for Live {
            fn allows(&self, _: Effects) -> bool {
                true
            }
        }
        let scope = Scope::assistant(1544, std::sync::Arc::new(Live));
        let mut s = Session::new(Project::default());
        let calls = [
            ("annotation.newLayer", json!({})),
            ("annotation.resume", json!({})),
            ("annotation.setClass", json!({"class":"vehicle", "color":"#E53444"})),
            ("annotation.addBox", json!({"box":spec(), "class":"vehicle"})),
            ("annotation.updateBox", json!({"row":0, "box":spec(), "class":"building"})),
        ];
        for (id, params) in calls {
            s.run_scoped(scope.clone(), &Call::command(id, params)).unwrap();
        }
        let layer = s.map().layer(s.active_layer.unwrap()).unwrap();
        assert_eq!(layer.dataset.features.len(), 1);
        assert_eq!(layer.dataset.value(0, layer.dataset.field_index("class").unwrap()), &Cell::Text("building".into()));
        // User data that resembles a credential-bearing address is retained verbatim in
        // replayable command output, rather than silently changing the annotation class/frame.
        let mut b = spec();
        b.frame.image = Some("https://example.invalid/image?token=user-data".into());
        let params = json!({"box":b, "class":"https://example.invalid/class?token=user-data"});
        let (clean, _) = crate::roles::command("annotation.addBox", &params, 0);
        assert_eq!(clean.as_ref().unwrap_or(&params), &params);
    }

    #[test]
    fn annotation_classes_color_existing_and_future_boxes_and_roundtrip() {
        let mut s = Session::new(Project::default());
        s.run("annotation.newLayer", &json!({})).unwrap();
        let id = s.active_layer.unwrap();
        // Accepted field names are case-insensitive, including imported class renderers.
        let l = s.map_mut().layer_mut(id).unwrap();
        for field in &mut l.dataset.fields {
            field.name = field.name.to_uppercase();
        }
        if let rustgis_map::Renderer::Unique { field, .. } = &mut l.renderer {
            *field = "CLASS".into();
        }
        for name in ["vehicle", "building", "vehicle"] {
            s.run("annotation.addBox", &json!({"box":spec(),"class":name})).unwrap();
        }
        let symbols = |s: &Session| {
            let l = s.map().layer(id).unwrap();
            (0..3).map(|row| l.renderer.symbol_for(&l.dataset, row)).collect::<Vec<_>>()
        };
        let before = symbols(&s);
        assert_eq!(before[0], before[2]);
        assert_ne!(before[0].stroke, before[1].stroke);
        s.run("annotation.setClass", &json!({"class":"vehicle","color":"#E53444"})).unwrap();
        let changed = symbols(&s);
        assert_eq!(changed[0].stroke.hex(), "#E53444");
        assert_eq!(changed[0], changed[2]);
        assert_eq!(changed[1], before[1]);
        s.run("edit.undo", &json!({})).unwrap();
        assert_eq!(symbols(&s), before);
        s.run("annotation.setClass", &json!({"class":"tree","color":"#22AA55"})).unwrap();
        let project: crate::Project = serde_json::from_str(&serde_json::to_string(&s.project).unwrap()).unwrap();
        let read = Session::new(project);
        assert_eq!(class_colors(&read, id), class_colors(&s, id));
        s.map_mut().editing.settings.enabled = false;
        s.tool = crate::Tool::Explore;
        s.run("annotation.resume", &json!({"layer":id})).unwrap();
        assert!(s.map().editing.settings.enabled);
        assert_eq!(s.tool, crate::Tool::RotatedBox);
        s.run("annotation.addBox", &json!({"box":spec(),"class":"tree"})).unwrap();
        let l = s.map().layer(id).unwrap();
        assert_eq!(l.dataset.features.len(), 4);
        assert_eq!(l.renderer.symbol_for(&l.dataset, 3).stroke.hex(), "#22AA55");
        let (bytes, loss) = rustgis_data::io::write_bytes(&l.dataset, "geojson").unwrap();
        assert_eq!(loss, 0);
        let (ds, _) = rustgis_data::io::read_bytes("annotations.geojson", &bytes).unwrap();
        assert_eq!(ds.value(3, ds.field_index("class").unwrap()), &Cell::Text("tree".into()));
        let imported = crate::cmd::layer::add_dataset(&mut s, ds);
        s.run("annotation.resume", &json!({"layer":imported})).unwrap();
        assert_eq!(class_colors(&s, imported).len(), 3, "GeoJSON classes get distinct layer styles on resume");
    }
    #[test]
    fn annotation_projection_cache_tracks_crs_without_changing_records() {
        let f = Frame { crs: Crs::Epsg(32631), ..Frame::default() };
        let first = f.transformer().unwrap();
        assert!(Rc::ptr_eq(&first, &f.transformer().unwrap()));
        let point = f.to_lonlat([500_000., -5_000_000.]).unwrap();
        let local = f.to_local(point).unwrap();
        assert!((local[0] - 500_000.).abs() < 1e-5 && (local[1] + 5_000_000.).abs() < 1e-5);
        let merc = Frame::default();
        assert!(!Rc::ptr_eq(&first, &merc.transformer().unwrap()));
        assert_eq!(merc.to_lonlat([0., 0.]), Some(Coord::new(0., 0.)));
        let read: Frame = serde_json::from_str(&serde_json::to_string(&f).unwrap()).unwrap();
        assert_eq!(read, f);
        assert_eq!(read.to_lonlat([500_000., -5_000_000.]), Some(point));
    }
    #[test]
    fn annotation_corner_approximation_is_reported_at_construction() {
        let mut s = Session::new(Project::default());
        let mark = s.notice_mark();
        s.run("annotation.newLayer", &json!({})).unwrap();
        assert!(s.notices_since(mark).is_empty());
        s.run("annotation.addBox", &json!({"box":spec()})).unwrap();
        let notices = s.notices_since(mark);
        assert_eq!(notices.iter().filter(|n| n.text.contains("1 annotation box") && n.text.contains("row 0")).count(), 1);
        let mark = s.notice_mark();
        s.run("annotation.updateBox", &json!({"row":0,"box":spec(),"class":"car"})).unwrap();
        assert!(s.notices_since(mark).is_empty());
    }
    #[test]
    fn annotation_delete_is_scoped_undoable_and_rejects_ambiguous_identity() {
        let mut s = Session::new(Project::default());
        s.run("annotation.newLayer", &json!({"name":"Other"})).unwrap();
        let other = s.active_layer.unwrap();
        s.run("annotation.addBox", &json!({"box":spec()})).unwrap();
        s.run("annotation.newLayer", &json!({"name":"Target"})).unwrap();
        let id = s.active_layer.unwrap();
        for _ in 0..3 {
            s.run("annotation.addBox", &json!({"box":spec(),"class":"vehicle"})).unwrap();
        }
        s.run("select.rows", &json!({"layer":other,"rows":[0]})).unwrap();
        s.run("select.rows", &json!({"layer":id,"rows":[0,1,2],"mode":"add"})).unwrap();
        let palette = class_colors(&s, id);
        let before = s.map().layer(id).unwrap().clone();
        let fi = before.dataset.field_index("annotation_id").unwrap();
        let uid = before.dataset.value(1, fi).to_string();
        s.run("annotation.delete", &json!({"layer":id,"annotation_id":uid})).unwrap();
        let l = s.map().layer(id).unwrap();
        assert_eq!(l.dataset.features.len(), 2);
        assert_eq!(l.selection, vec![0, 1]);
        assert_eq!(s.map().layer(other).unwrap().selection, vec![0]);
        assert_eq!(class_colors(&s, id), palette);
        assert!(s.undo());
        assert_eq!(s.map().layer(id).unwrap().dataset.features, before.dataset.features);
        assert!(s.run("annotation.delete", &json!({"layer":id,"annotation_id":"missing"})).is_err());
        s.map_mut().layer_mut(id).unwrap().dataset.features.get_mut(0).unwrap().attrs[fi] = Cell::Text(uid.clone());
        let unchanged = serde_json::to_string(&s.project).unwrap();
        assert!(s.run("annotation.delete", &json!({"layer":id,"annotation_id":uid})).is_err());
        assert_eq!(serde_json::to_string(&s.project).unwrap(), unchanged);
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
                affine: None,
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
