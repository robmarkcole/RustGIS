//! One TIFF and its sibling GeoJSON: original pixels, stable categories, and OBB corners.
use super::*;
use rustgis_data::annotation::{Category, Image, Metadata};
use rustgis_map::Layer;
use std::path::{Path, PathBuf};

pub fn commands() -> Vec<C> {
    use crate::policy::{Effect, Effects};
    vec![
        C::new("annotation.openImage", "Annotate Image", "Annotate › Image", open)
            .inputs(open_inputs)
            .roles(&[])
            .effects(Effects::NONE.with(Effect::Files))
            .params(r#"{"layer"?:loaded TIFF or image annotation layer,"path"?:"*.tif|*.tiff|*.geojson"}"#),
        C::new("annotation.saveImage", "Save Annotations", "Annotate › Image", save)
            .inputs(super::super::Inputs::single_layer)
            .roles(&[])
            .effects(Effects::NONE.with(Effect::Files))
            .view()
            .when(need_bound)
            .params(r#"{"layer"?:annotation layer,"path"?:"*.geojson (default: associated sibling; explicit path writes a copy without rebinding)"}"#),
        C::new("annotation.importClasses", "Import Annotation Classes", "Annotate › Classes", import_classes)
            .inputs(super::super::Inputs::single_layer)
            .roles(&[crate::ParamRole { key: "text", role: crate::Role::Opaque }])
            .effects(Effects::NONE.with(Effect::Files))
            .when(need_bound)
            .params(r#"{"layer"?:image annotation layer,"path"?:"*.geojson","text"?:GeoJSON text}"#),
        C::new("annotation.obb", "Image Annotation Boxes", "Annotate › Image", obb)
            .inputs(super::super::Inputs::single_layer)
            .roles(&[])
            .effects(Effects::NONE)
            .view()
            .when(need_bound)
            .params(r#"{"layer"?:image annotation layer}"#),
    ]
}
fn need_bound(s: &Session) -> Option<&'static str> {
    s.active_layer
        .and_then(|id| s.map().layer(id))
        .and_then(|l| l.dataset.annotation.as_ref())
        .is_none()
        .then_some("select an image-bound annotation layer")
}
fn open_inputs(input: &super::super::InputContext<'_>, p: &Value) -> Result<super::super::Inputs, String> {
    if ps(p, "path").is_some() {
        Ok(super::super::Inputs::layers(std::iter::empty()))
    } else {
        super::super::Inputs::single_layer(input, p)
    }
}
pub fn frame(image: &Image) -> Frame {
    Frame { crs: image.crs, transform: image.transform(), image: Some(image.file_name.clone()), affine: Some(image.affine) }
}
pub fn bound_frame(s: &Session, id: u32) -> Option<Frame> {
    s.map().layer(id)?.dataset.annotation.as_ref().map(|m| frame(&m.image))
}
/// Frame names are presentation; grid identity determines whether the same pixel coordinates apply.
pub fn check_frame(s: &Session, id: u32, f: &Frame) -> Result<(), String> {
    if let Some(expected) = bound_frame(s, id)
        && (f.crs != expected.crs || f.affine != expected.affine)
    {
        return Err("annotation must use its bound image's original pixel grid".into());
    }
    Ok(())
}
pub fn sync_palette(l: &mut Layer) -> Result<(), String> {
    let colors = match &l.renderer {
        rustgis_map::Renderer::Unique { field, classes, expression: None, .. } if field.eq_ignore_ascii_case("class") => {
            classes.iter().map(|(n, s)| (n.clone(), [s.stroke.0[0], s.stroke.0[1], s.stroke.0[2]])).collect::<Vec<_>>()
        }
        _ => return Ok(()),
    };
    if let Some(m) = &mut l.dataset.annotation {
        m.validate()?;
        for (name, color) in colors {
            if let Some(c) = m.categories.iter_mut().find(|c| c.name == name) {
                c.color = color;
            } else {
                let id = m.categories.iter().map(|c| c.id).max().unwrap_or(0).checked_add(1).ok_or("category IDs exhausted")?;
                m.categories.push(Category { id, name, color });
            }
        }
        m.validate()?;
    }
    Ok(())
}
pub fn restore(l: &mut Layer) -> Result<(), String> {
    let Some(m) = l.dataset.annotation.clone() else { return Ok(()) };
    m.validate()?;
    l.renderer = rustgis_map::Renderer::unique(&l.dataset, &[], "class", rustgis_map::Ramp::Harbour, l.renderer.base_symbol())?;
    // Clear generated classes: the exported category table includes unused classes and controls order.
    if let rustgis_map::Renderer::Unique { classes, .. } = &mut l.renderer {
        classes.clear();
    }
    for c in m.categories {
        ensure_class(l, &c.name, Some(rustgis_map::Rgba::rgb(c.color[0], c.color[1], c.color[2])))?;
    }
    Ok(())
}
fn image_of(s: &Session, id: u32) -> Result<Image, String> {
    let rl = s.map().layer(id).and_then(|l| l.raster.as_ref()).ok_or("select a loaded TIFF image")?;
    let r = rl.data.as_ref().ok_or("load the image before annotating")?;
    if rl.crs.is_some() || rl.subdataset.is_some() {
        return Err(
            "image binding requires the original GeoTIFF grid without CRS overrides or subdatasets; export a corrected GeoTIFF first"
                .into(),
        );
    }
    if rl.in_place || r.overview.is_some() {
        return Err("image annotation requires the full TIFF, not an overview or streamed image".into());
    }
    let name = rl
        .path
        .as_deref()
        .or(rl.file_name.as_deref())
        .and_then(|p| Path::new(p).file_name())
        .and_then(|n| n.to_str())
        .ok_or("image filename is unavailable")?;
    if !matches!(Path::new(name).extension().and_then(|s| s.to_str()).map(str::to_ascii_lowercase).as_deref(), Some("tif" | "tiff")) {
        return Err("image annotation requires a TIFF".into());
    }
    if let Some(bytes) = &rl.embedded {
        return rustgis_data::raster::geotiff::annotation_image(std::io::Cursor::new(bytes.as_slice()), name).map_err(|e| e.to_string());
    }
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(path) = &rl.path {
        return rustgis_data::raster::geotiff::annotation_image(std::fs::File::open(path).map_err(|e| e.to_string())?, name)
            .map_err(|e| e.to_string());
    }
    Err("image source bytes are unavailable; upload the original TIFF".into())
}
fn local(path: &Path) -> Result<(), String> {
    if !rustgis_map::paths::is_local(&path.to_string_lossy()) {
        return Err("choose a local TIFF/GeoJSON pair".into());
    }
    Ok(())
}
fn sibling_image(geo: &Path) -> Result<PathBuf, String> {
    let candidates = ["tif", "tiff", "TIF", "TIFF"].map(|ext| geo.with_extension(ext));
    let mut found = Vec::new();
    for p in candidates {
        if p.is_file() && !found.iter().any(|q: &PathBuf| same_path(q, &p)) {
            found.push(p);
        }
    }
    match found.as_slice() {
        [p] => Ok(p.clone()),
        [] => {
            // Save As may give the GeoJSON another basename. Only a validated basename from its
            // binding is followed, inside this same directory; metadata never names a path/URL.
            let bytes = crate::project::read_file(geo, rustgis_data::io::MAX_BYTES)?;
            let (ds, _) = rustgis_data::io::read_bytes(&geo.to_string_lossy(), &bytes).map_err(|e| e.to_string())?;
            let meta = ds.annotation.ok_or("no sibling TIFF or image binding found")?;
            let image = geo.with_file_name(meta.image.file_name);
            if image.is_file() { Ok(image) } else { Err("no sibling TIFF found; load the image and select Annotate Image".into()) }
        }
        _ => Err("multiple sibling TIFFs found; select the intended image explicitly".into()),
    }
}
fn same_path(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}
fn open(s: &mut Session, p: &Value) -> CmdResult {
    let mut geo = None;
    let mut added_raster = false;
    let raster = if let Some(path) = ps(p, "path") {
        let path = Path::new(path);
        local(path)?;
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        let image = if ext == "geojson" {
            geo = Some(path.to_path_buf());
            sibling_image(path)?
        } else if matches!(ext.as_str(), "tif" | "tiff") {
            path.to_path_buf()
        } else {
            return Err("choose a TIFF or image annotation GeoJSON".into());
        };
        if let Some(id) = s
            .map()
            .layers
            .iter()
            .find(|l| l.raster.as_ref().and_then(|r| r.path.as_deref()).is_some_and(|p| same_path(Path::new(p), &image)))
            .map(|l| l.id)
        {
            id
        } else {
            added_raster = true;
            let result = super::super::raster::add_path(s, &image.to_string_lossy(), None, false)?;
            result.get("layer").and_then(Value::as_u64).and_then(|i| u32::try_from(i).ok()).ok_or("image could not be loaded")?
        }
    } else {
        let id = s.layer_id(p)?;
        if s.map().layer(id).is_some_and(|l| l.dataset.annotation.is_some()) {
            let layer = s.map().layer(id).ok_or("no layer")?;
            let source = layer.dataset.source.as_deref().ok_or("import this GeoJSON from a file, or select its TIFF")?;
            return open(s, &json!({"path":source}));
        }
        id
    };
    let image = image_of(s, raster)?;
    let path = s
        .map()
        .layer(raster)
        .and_then(|l| l.raster.as_ref())
        .and_then(|r| r.path.as_deref())
        .map(|p| Path::new(p).with_extension("geojson"));
    let geo = geo.or(path);
    if let Some(id) = s
        .map()
        .layers
        .iter()
        .find(|l| {
            l.dataset
                .annotation
                .as_ref()
                .is_some_and(|m| m.image.same_grid(&image) && (geo.is_some() || m.image.file_name == image.file_name))
                && geo.as_ref().is_none_or(|g| l.dataset.source.as_ref().is_some_and(|p| same_path(Path::new(p), g)))
        })
        .map(|l| l.id)
    {
        if let Some(m) = s.map_mut().layer_mut(id).and_then(|l| l.dataset.annotation.as_mut()) {
            m.image.file_name = image.file_name;
        }
        // A TIFF discovered after importing its GeoJSON must sit below those annotations.
        if added_raster {
            let from = s.map().layer_index(raster).ok_or("no image layer")?;
            let to = s.map().layer_index(id).ok_or("no annotation layer")?;
            if from < to {
                s.map_mut().move_layer(from, to)?;
            }
        }
        s.zoom_to(&s.map().layer(raster).ok_or("no image")?.extent());
        return super::resume(s, &json!({"layer":id}));
    }
    let mut ds = if let Some(path) = &geo
        && path.exists()
    {
        let bytes = crate::project::read_file(path, rustgis_data::io::MAX_BYTES)?;
        let (ds, report) = rustgis_data::io::read_bytes(&path.to_string_lossy(), &bytes).map_err(|e| e.to_string())?;
        if report.skipped > 0 || !report.messages.is_empty() {
            return Err("annotation GeoJSON has import losses; repair it before continuing".into());
        }
        let meta = ds.annotation.as_ref().ok_or("sibling GeoJSON has no image annotation binding; it was left unchanged")?;
        if !meta.image.same_grid(&image) {
            return Err("sibling GeoJSON image dimensions or pixel grid do not match this TIFF".into());
        }
        ds
    } else {
        let mut ds = Dataset::new(
            format!("{} annotations", Path::new(&image.file_name).file_stem().and_then(|n| n.to_str()).unwrap_or("Image")),
            GeometryType::Polygon,
            vec![
                Field::new("annotation_id", FieldType::Text),
                Field::new("class", FieldType::Text),
                Field::new(RECORD, FieldType::Text),
                Field::new(SHAPE_RECORD, FieldType::Text),
            ],
        );
        ds.annotation = Some(Metadata { version: 1, image: image.clone(), categories: Vec::new() });
        ds
    };
    if let Some(m) = &mut ds.annotation {
        m.image.file_name = image.file_name;
    }
    ds.source = geo.map(|p| std::path::absolute(&p).unwrap_or(p).to_string_lossy().into_owned());
    let id = super::super::layer::add_dataset(s, ds);
    restore(s.map_mut().layer_mut(id).ok_or("no layer")?)?;
    validate(s, id)?;
    s.zoom_to(&s.map().layer(raster).ok_or("no image")?.extent());
    super::resume(s, &json!({"layer":id}))
}
pub fn validate(s: &Session, id: u32) -> Result<(), String> {
    let ds = &s.map().layer(id).ok_or("no layer")?.dataset;
    let Some(meta) = &ds.annotation else { return Ok(()) };
    meta.validate()?;
    let mut ids = std::collections::BTreeSet::new();
    for row in 0..ds.features.len() {
        let uid = ds.field_index("annotation_id").map(|fi| ds.value(row, fi));
        if !matches!(uid,Some(Cell::Text(t)) if !t.is_empty() && ids.insert(t.clone())) {
            return Err("image annotations need unique nonempty annotation IDs".into());
        }
        let f = if let Ok(b) = BoxSpec::from_row(ds, row) { b.frame } else { ShapeSpec::from_row(ds, row)?.frame };
        check_frame(s, id, &f)?;
        let class = ds.field_index("class").map(|fi| ds.value(row, fi));
        if !matches!(class,Some(Cell::Text(t)) if meta.categories.iter().any(|c|c.name==*t)) {
            return Err("feature class is absent from the category table".into());
        }
    }
    Ok(())
}
pub fn export_dataset(l: &Layer) -> Result<Dataset, String> {
    let mut copy = l.clone();
    sync_palette(&mut copy)?;
    Ok(copy.dataset)
}
fn save(s: &mut Session, p: &Value) -> CmdResult {
    let id = s.layer_id(p)?;
    validate(s, id)?;
    let l = s.map().layer(id).ok_or("no layer")?;
    if l.dataset.annotation.is_none() {
        return Err("select an image-bound annotation layer".into());
    }
    let path =
        ps(p, "path").map(String::from).or_else(|| l.dataset.source.clone()).ok_or("choose a GeoJSON destination using Export Features")?;
    if Path::new(&path).extension().and_then(|e| e.to_str()).is_none_or(|e| !e.eq_ignore_ascii_case("geojson")) {
        return Err("image annotations must be saved as .geojson".into());
    }
    local(Path::new(&path))?;
    // An explicit destination writes a copy without rebinding dataset.source. Only the
    // already-associated companion may be overwritten, whether or not a path was supplied.
    if Path::new(&path).exists() && l.dataset.source.as_ref().is_none_or(|src| !same_path(Path::new(src), Path::new(&path))) {
        return Err("destination exists; choose a new GeoJSON filename".into());
    }
    let ds = export_dataset(l)?;
    let (bytes, _) = rustgis_data::io::write_bytes(&ds, "geojson").map_err(|e| e.to_string())?;
    #[cfg(target_arch = "wasm32")]
    {
        let _ = bytes;
        return Err("use Export Features to download image annotations in the browser".into());
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        rustgis_data::io::write_atomic(Path::new(&path), &bytes).map_err(|e| e.to_string())?;
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        s.notify(format!("Saved image annotations to {path}"), false);
        Ok(json!({"path":path,"features":ds.features.len()}))
    }
}
fn import_classes(s: &mut Session, p: &Value) -> CmdResult {
    let id = s.layer_id(p)?;
    check_layer(s, id)?;
    if s.map().layer(id).and_then(|l| l.dataset.annotation.as_ref()).is_none() {
        return Err("select an image-bound annotation layer".into());
    }
    let text = super::super::items::read_text(p)?;
    let (ds, _) = rustgis_data::io::read_bytes("classes.geojson", text.as_bytes()).map_err(|e| e.to_string())?;
    let m = ds.annotation.ok_or("GeoJSON has no annotation category table")?;
    let l = s.map_mut().layer_mut(id).ok_or("no layer")?;
    let target = l.dataset.annotation.as_mut().ok_or("no image binding")?;
    for c in m.categories {
        if target.categories.iter().any(|old| (old.id == c.id) != (old.name == c.name)) {
            return Err("category ID/name conflict; reconcile category identities before importing".into());
        }
        if let Some(old) = target.categories.iter_mut().find(|old| old.id == c.id) {
            *old = c;
        } else {
            target.categories.push(c);
        }
    }
    target.validate()?;
    restore(l)?;
    Ok(json!({"categories":l.dataset.annotation.as_ref().map(|m|m.categories.len())}))
}
fn obb(s: &mut Session, p: &Value) -> CmdResult {
    let id = s.layer_id(p)?;
    validate(s, id)?;
    let ds = &s.map().layer(id).ok_or("no layer")?.dataset;
    if ds.annotation.is_none() {
        return Err("OBB output requires an image-bound annotation layer".into());
    }
    let total = ds.features.len();
    let geo = rustgis_data::io::to_geojson(ds);
    let boxes = geo
        .get("features")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .filter(|(row, _)| BoxSpec::from_row(ds, *row).is_ok())
        .filter_map(|(_, f)| f.get("properties").filter(|p| p.get("obb").is_some()).cloned())
        .collect::<Vec<_>>();
    let skipped = total.saturating_sub(boxes.len());
    if skipped > 0 {
        s.notify(format!("Layer {id}: {skipped} annotation features omitted from OBB output (only boxes have OBB corners)"), false);
    }
    Ok(
        json!({"boxes":boxes,"count":boxes.len(),"total":total,"skipped":skipped,"units":"original image pixels","cornerOrder":"local top-left, top-right, bottom-right, bottom-left"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Project;
    use rustgis_data::raster::{ColorHint, Raster};
    fn fixture() -> (PathBuf, PathBuf) {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "rustgis-image-annotation-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let tif = dir.join("tile.tif");
        let r = Raster::new(
            "tile",
            64,
            48,
            vec![vec![10.; 64 * 48]],
            GeoTransform { origin_x: 400_000., origin_y: 5_000_000., pixel_width: 0.5, pixel_height: -1. },
            Crs::Epsg(32633),
            None,
            ColorHint::Gray,
            "Float32",
        )
        .unwrap();
        std::fs::write(&tif, rustgis_data::raster::geotiff::write(&r, 0).unwrap()).unwrap();
        (dir, tif)
    }
    fn new_session(tif: &Path) -> (Session, u32, Frame) {
        let mut s = Session::new(Project::new("Images"));
        let r = s.run("annotation.openImage", &json!({"path":tif})).unwrap();
        let id = r["layer"].as_u64().unwrap() as u32;
        let f = bound_frame(&s, id).unwrap();
        (s, id, f)
    }
    fn add_box(s: &mut Session, f: Frame) {
        let b = BoxSpec { version: 1, frame: f, center: [20., 18.], width: 16., height: 6., angle: 37. };
        s.run("annotation.addBox", &json!({"box":b,"class":"vehicle"})).unwrap();
    }
    #[test]
    fn annotation_obb_reports_omissions_once_and_preserves_source_features() {
        fn query(s: &mut Session, total: usize, count: usize) {
            let before = serde_json::to_value(&s.project).unwrap();
            let mark = s.notice_mark();
            let out = s.run("annotation.obb", &json!({})).unwrap();
            assert_eq!(out["total"], total);
            assert_eq!(out["count"], count);
            assert_eq!(out["skipped"], total - count);
            assert_eq!(out["boxes"].as_array().unwrap().len(), count);
            let notices = s.notices_since(mark);
            if total == count {
                assert!(notices.is_empty());
            } else {
                assert_eq!(notices.len(), 1);
                assert!(!notices[0].error);
                assert!(notices[0].text.contains(&format!("{} annotation features omitted", total - count)));
                assert!(notices[0].text.contains("only boxes have OBB corners"));
            }
            assert_eq!(serde_json::to_value(&s.project).unwrap(), before);
        }
        let (dir, tif) = fixture();
        let (mut s, id, f) = new_session(&tif);
        query(&mut s, 0, 0);
        add_box(&mut s, f.clone());
        query(&mut s, 1, 1);
        for shape in [
            Shape::Circle { center: [40., 30.], radius: 4. },
            Shape::Polygon { vertices: vec![[2., 2.], [8., 2.], [8., 8.], [2., 8.]] },
            Shape::Point { position: [52., 35.] },
        ] {
            s.run("annotation.addShape", &json!({"shape":ShapeSpec{version:1,frame:f.clone(),shape},"class":"vehicle"})).unwrap();
        }
        // An external attribute named `obb` is not evidence of a constrained box.
        let l = s.map_mut().layer_mut(id).unwrap();
        let oi = l.dataset.ensure_field("obb", FieldType::Text);
        l.dataset.features.get_mut(1).unwrap().attrs[oi] = Cell::Text("external attribute".into());
        query(&mut s, 4, 1);
        let ds = &s.map().layer(id).unwrap().dataset;
        let uid = ds.value(0, ds.field_index("annotation_id").unwrap()).to_string();
        s.run("annotation.delete", &json!({"annotation_id":uid})).unwrap();
        query(&mut s, 3, 0);
        drop(s);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn annotation_save_copy_preserves_binding_and_later_saves_target_original() {
        let (dir, tif) = fixture();
        let (mut s, id, f) = new_session(&tif);
        add_box(&mut s, f);
        s.run("annotation.saveImage", &json!({})).unwrap();
        let original = tif.with_extension("geojson");
        let source = s.map().layer(id).unwrap().dataset.source.clone();
        let original_bytes = std::fs::read(&original).unwrap();
        let copy = dir.join("copy.geojson");
        s.run("annotation.saveImage", &json!({"path":copy})).unwrap();
        let copy_bytes = std::fs::read(&copy).unwrap();
        assert_eq!(copy_bytes, original_bytes);
        assert_eq!(s.map().layer(id).unwrap().dataset.source, source);
        assert!(s.run("annotation.saveImage", &json!({"path":copy})).is_err());
        assert_eq!(std::fs::read(&copy).unwrap(), copy_bytes);
        assert_eq!(std::fs::read(&original).unwrap(), original_bytes);
        let mut b = BoxSpec::from_row(&s.map().layer(id).unwrap().dataset, 0).unwrap();
        b.width = 20.;
        s.run("annotation.updateBox", &json!({"row":0,"box":b})).unwrap();
        s.run("annotation.saveImage", &json!({})).unwrap();
        assert_ne!(std::fs::read(&original).unwrap(), original_bytes);
        assert_eq!(std::fs::read(&copy).unwrap(), copy_bytes);
        assert_eq!(s.map().layer(id).unwrap().dataset.source, source);
        drop(s);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn annotation_image_pair_roundtrip_preserves_palette_ids_shapes_and_obb() {
        let (dir, tif) = fixture();
        let (mut s, id, f) = new_session(&tif);
        s.run("annotation.setClass", &json!({"class":"vehicle","color":"#123456"})).unwrap();
        s.run("annotation.setClass", &json!({"class":"unused","color":"#AA3300"})).unwrap();
        add_box(&mut s, f.clone());
        s.run(
            "annotation.addShape",
            &json!({"shape":ShapeSpec{version:1,frame:f,shape:Shape::Circle{center:[40.,30.],radius:4.}},"class":"vehicle"}),
        )
        .unwrap();
        let original = &s.map().layer(id).unwrap().dataset;
        let uid = original.value(0, original.field_index("annotation_id").unwrap()).clone();
        s.run("annotation.saveImage", &json!({})).unwrap();
        let raw: Value = serde_json::from_slice(&std::fs::read(tif.with_extension("geojson")).unwrap()).unwrap();
        assert_eq!(raw["rustgis_annotation"]["categories"].as_array().unwrap().len(), 2);
        assert_eq!(raw["features"][0]["properties"]["category_id"], 1);
        let corners = raw["features"][0]["properties"]["obb"].as_array().unwrap();
        assert_eq!(corners.len(), 4);
        let p = |i: usize| [corners[i][0].as_f64().unwrap(), corners[i][1].as_f64().unwrap()];
        let (a, b, c) = (p(0), p(1), p(2));
        let dot = (b[0] - a[0]) * (c[0] - b[0]) + (b[1] - a[1]) * (c[1] - b[1]);
        assert!(dot.abs() < 1e-10);
        assert!(raw["features"][1]["properties"].get("obb").is_none());
        assert_eq!(s.run("annotation.obb", &json!({})).unwrap()["count"], 1);
        let mut reopened = Session::new(Project::new("Again"));
        reopened.run("annotation.openImage", &json!({"path":tif.with_extension("geojson")})).unwrap();
        let layer = reopened.map().layer(reopened.active_layer.unwrap()).unwrap();
        assert_eq!(layer.dataset.value(0, layer.dataset.field_index("annotation_id").unwrap()), &uid);
        assert_eq!(class_colors(&reopened, layer.id), class_colors(&s, id));
        assert_eq!(layer.dataset.annotation, s.map().layer(id).unwrap().dataset.annotation);
        assert!(ShapeSpec::from_row(&layer.dataset, 1).is_ok());
        let mut through_add_data = Session::new(Project::new("GeoJSON first"));
        through_add_data.run("layer.addData", &json!({"path":tif.with_extension("geojson")})).unwrap();
        through_add_data.run("annotation.openImage", &json!({})).unwrap();
        assert_eq!(through_add_data.map().layers.len(), 2);
        assert!(through_add_data.map().layers[0].dataset.annotation.is_some(), "discovered TIFF must not obscure imported annotations");
        let project = serde_json::to_string(&reopened.project).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&project).unwrap()["format"], 2);
        let back = Project::from_json(&project).unwrap();
        assert_eq!(back.map().layers.iter().find(|l| l.dataset.annotation.is_some()).unwrap().dataset.annotation, layer.dataset.annotation);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn annotation_image_empty_palette_import_undo_conflicts_and_future_versions() {
        let (dir, tif) = fixture();
        let (mut s, id, _) = new_session(&tif);
        s.run("annotation.setClass", &json!({"class":"empty","color":"#010203"})).unwrap();
        let before = s.map().layer(id).unwrap().dataset.annotation.clone();
        let mut geo = rustgis_data::io::to_geojson(&s.map().layer(id).unwrap().dataset);
        geo["rustgis_annotation"]["categories"] = json!([{"id":42,"name":"new","color":[9,8,7]}]);
        s.run("annotation.importClasses", &json!({"text":geo.to_string()})).unwrap();
        assert_eq!(s.map().layer(id).unwrap().dataset.annotation.as_ref().unwrap().categories[1].id, 42);
        s.undo();
        assert_eq!(s.map().layer(id).unwrap().dataset.annotation, before);
        geo["rustgis_annotation"]["categories"] =
            json!([{"id":42,"name":"new","color":[9,8,7]},{"id":1,"name":"conflict","color":[9,8,7]}]);
        assert!(s.run("annotation.importClasses", &json!({"text":geo.to_string()})).is_err());
        assert_eq!(s.map().layer(id).unwrap().dataset.annotation, before);
        s.run("annotation.saveImage", &json!({})).unwrap();
        let (read, _) = rustgis_data::io::read_bytes("tile.geojson", &std::fs::read(tif.with_extension("geojson")).unwrap()).unwrap();
        assert!(read.features.is_empty());
        assert_eq!(read.annotation, before);
        assert!(read.field_index(RECORD).is_some());
        let copy = dir.join("copy.geojson");
        s.run("annotation.saveImage", &json!({"path":copy})).unwrap();
        let mut copied = Session::new(Project::new("Copy"));
        copied.run("annotation.openImage", &json!({"path":copy})).unwrap();
        assert_eq!(copied.map().layers.len(), 2);
        std::fs::copy(&tif, tif.with_extension("tiff")).unwrap();
        let mut ambiguous = Session::new(Project::new("Ambiguous"));
        assert!(ambiguous.run("annotation.openImage", &json!({"path":tif.with_extension("geojson")})).is_err());

        let mut bad_type = geo.clone();
        bad_type["type"] = json!("Feature");
        bad_type["geometry"] = Value::Null;
        bad_type["properties"] = json!({});
        assert!(rustgis_data::io::read_bytes("tile.geojson", bad_type.to_string().as_bytes()).is_err());
        let mut bad_path = geo.clone();
        bad_path["rustgis_annotation"]["image"]["file_name"] = json!("C:outside.tif");
        assert!(rustgis_data::io::read_bytes("tile.geojson", bad_path.to_string().as_bytes()).is_err());
        geo["rustgis_annotation"]["version"] = json!(99);
        assert!(rustgis_data::io::read_bytes("tile.geojson", geo.to_string().as_bytes()).is_err());
        for kind in ["map", "layer", "mapPackage"] {
            let (_, bytes) = super::super::super::items::item_bytes(&mut s, kind, &json!({})).unwrap();
            let text = if kind == "mapPackage" {
                super::super::super::items::package_text(&bytes).unwrap()
            } else {
                String::from_utf8(bytes).unwrap()
            };
            assert_eq!(serde_json::from_str::<Value>(&text).unwrap()["format"], crate::project::FORMAT);
        }
        let mut saved = serde_json::to_value(&s.project).unwrap();
        let index = s.map().layer_index(id).unwrap();
        saved["maps"][0]["layers"][index]["dataset"]["annotation"]["version"] = json!(99);
        assert!(Project::from_json(&saved.to_string()).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn annotation_image_binding_rejects_wrong_grid_and_preserves_renamed_pairs() {
        let (dir, tif) = fixture();
        let (mut s, id, f) = new_session(&tif);
        add_box(&mut s, f.clone());
        let mut wrong = f;
        wrong.affine = None;
        assert!(
            s.run(
                "annotation.addBox",
                &json!({"box":BoxSpec{version:1,frame:wrong,center:[10.,10.],width:2.,height:3.,angle:0.},"class":"bad"})
            )
            .is_err()
        );
        assert_eq!(s.map().layer(id).unwrap().dataset.features.len(), 1);
        s.run("annotation.saveImage", &json!({})).unwrap();
        let geo = tif.with_extension("geojson");
        std::fs::rename(&tif, dir.join("renamed.tif")).unwrap();
        std::fs::rename(&geo, dir.join("renamed.geojson")).unwrap();
        let (again, id, _) = new_session(&dir.join("renamed.tif"));
        assert_eq!(again.map().layer(id).unwrap().dataset.annotation.as_ref().unwrap().image.file_name, "renamed.tif");
        let mut raw: Value = serde_json::from_slice(&std::fs::read(dir.join("renamed.geojson")).unwrap()).unwrap();
        raw["rustgis_annotation"]["image"]["width"] = json!(999);
        std::fs::write(dir.join("renamed.geojson"), raw.to_string()).unwrap();
        let mut other = Session::new(Project::new("Mismatch"));
        let layers = other.map().layers.len();
        assert!(other.run("annotation.openImage", &json!({"path":dir.join("renamed.tif")})).is_err());
        assert_eq!(other.map().layers.len(), layers);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn annotation_image_affine_frame_preserves_pixel_rectangles_and_inverse() {
        let image = Image {
            file_name: "skew.tif".into(),
            width: 20,
            height: 10,
            crs: Crs::Epsg(32633),
            affine: [400_000., 30., 5., 5_000_000., 3., -30.],
        };
        let f = frame(&image);
        for p in [[0., 0.], [20., 10.], [3.25, 7.5]] {
            let back = f.to_local(f.to_lonlat(p).unwrap()).unwrap();
            assert!((p[0] - back[0]).abs() < 1e-5 && (p[1] - back[1]).abs() < 1e-5);
        }
        let b = BoxSpec { version: 1, frame: f, center: [10., 5.], width: 8., height: 3., angle: 32. };
        assert!(b.geometry().is_ok());
    }
}
