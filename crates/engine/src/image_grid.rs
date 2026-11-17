//! Original GeoTIFF construction grids, read independently of display overviews.
use std::sync::Arc;

use crate::{CmdResult, Session, box_edit::Frame};
use rustgis_data::progress;
use rustgis_map::raster::RasterLayer;
use serde_json::Value;

/// Session-local identity of an image source. Style changes do not invalidate a grid; replacing
/// its pixels, file reference, embedded bytes, layer or map does. No file reads occur here.
#[derive(Clone)]
pub struct Source {
    map: u64,
    layer: u32,
    instance: u64,
    replacement: u64,
    raster: RasterLayer,
}
fn same_arc<T>(a: &Option<Arc<T>>, b: &Option<Arc<T>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}
impl Source {
    pub fn capture(s: &Session, layer: u32) -> Result<Self, String> {
        let l = s.map().layer(layer).ok_or("no image layer")?;
        let raster = l.raster.clone().ok_or("choose a GeoTIFF image layer")?;
        Ok(Self { map: s.map().instance_id(), layer, instance: l.instance_id(), replacement: l.replacement_revision(), raster })
    }
    pub fn current(&self, s: &Session) -> bool {
        s.map().instance_id() == self.map
            && s.map().layer(self.layer).is_some_and(|l| {
                l.instance_id() == self.instance
                    && l.replacement_revision() == self.replacement
                    && l.raster.as_ref().is_some_and(|r| {
                        r.path == self.raster.path && same_arc(&r.embedded, &self.raster.embedded) && same_arc(&r.data, &self.raster.data)
                    })
            })
    }
}

/// Cancel is checked before/after the bounded header parse; a filesystem read/seek itself
/// cannot be interrupted until the OS returns. No raster pixel decoding takes place.
pub(crate) fn read(r: &RasterLayer) -> Result<Frame, String> {
    progress::begin(2);
    progress::step(1).map_err(|e| e.to_string())?;
    let grid = if let Some(bytes) = &r.embedded {
        rustgis_data::raster::geotiff::read_image_grid(std::io::Cursor::new(bytes.as_slice()))
    } else {
        let path = r
            .path
            .as_deref()
            .filter(|p| rustgis_map::paths::is_local(p))
            .ok_or("choose a local or embedded GeoTIFF; remote original grids must be downloaded and added first")?;
        #[cfg(not(target_arch = "wasm32"))]
        {
            if rustgis_data::raster::gdal::is_gdal_raster_name(path) {
                return Err("Original image grids require a GeoTIFF source. Export this image as a GeoTIFF and add that file.".into());
            }
            let input = std::fs::File::open(path).map_err(|e| e.to_string())?;
            let displayed = r.data.as_ref().ok_or("The image pixels are not loaded. Use Reload Data, then retry the operation.")?;
            let path = std::path::Path::new(path);
            displayed.check_original_handle(path, &input)?;
            let grid = rustgis_data::raster::geotiff::read_image_grid(&input);
            // Both the handle and path must still name the revision that supplied the display.
            // A replacement during parsing must not pair new coordinates with old pixels.
            displayed.check_original_handle(path, &input)?;
            displayed.check_original_file(path)?;
            grid
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = path;
            return Err("upload the original GeoTIFF to use its grid in the browser".into());
        }
    }
    .map_err(|e| e.to_string())?;
    progress::step(1).map_err(|e| e.to_string())?;
    Frame::from_grid(&grid)
}
struct Prepared {
    source: Source,
    result: Result<Frame, String>,
}
pub(crate) fn ahead(s: &Session, p: &Value) -> Option<crate::cmd::reads::ReadWork> {
    let source = Source::capture(s, s.layer_id(p).ok()?).ok()?;
    Some(crate::cmd::reads::ahead("Read Original Image Grid", "imageGrid".into(), move || {
        let result = read(&source.raster);
        Prepared { source, result }
    }))
}
/// Desktop UI calls require the cancellable read queue. Headless callers run on their own
/// worker; the browser reads uploaded bytes inline. Reads only headers, never display pixels.
pub(crate) fn query(s: &mut Session, p: &Value) -> CmdResult {
    let source = Source::capture(s, s.layer_id(p)?)?;
    let ready = crate::cmd::reads::take::<Prepared>(s, "imageGrid");
    #[cfg(not(target_arch = "wasm32"))]
    if ready.is_none() && s.statistics_window_thread == Some(std::thread::current().id()) {
        return Err("original image grids need the desktop background read queue".into());
    }
    let frame = match ready {
        Some(done) if done.source.current(s) && done.source.layer == source.layer => done.result?,
        Some(_) => return Err("image source changed while reading its grid; select the image again".into()),
        None => read(&source.raster)?,
    };
    serde_json::to_value(frame).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster, geotiff};
    use serde_json::json;
    fn session() -> (Session, u32, Frame) {
        let raster = Raster::new(
            "original",
            4,
            6,
            vec![vec![1.; 24]],
            GeoTransform { origin_x: 100., origin_y: 500., pixel_width: 2., pixel_height: -3. },
            Crs::from_epsg(3857).unwrap(),
            None,
            ColorHint::Gray,
            "UInt8",
        )
        .unwrap();
        let bytes = geotiff::write(&raster, 0).unwrap();
        let frame = Frame::from_grid(&geotiff::read_image_grid(std::io::Cursor::new(&bytes)).unwrap()).unwrap();
        let overview = Raster::new("overview", 1, 1, vec![vec![1.]], raster.transform, raster.crs, None, ColorHint::Gray, "UInt8").unwrap();
        let mut s = Session::new(crate::Project::default());
        let id = s.map_mut().add_raster("Image", RasterLayer::from_bytes(overview, "image.tif", bytes));
        (s, id, frame)
    }
    #[test]
    fn annotation_grid_command_reads_original_header_and_never_mutates_the_project() {
        let (mut s, id, expected) = session();
        let depth = s.undo_depth();
        let frame: Frame = serde_json::from_value(s.run("raster.imageGrid", &json!({"layer":id})).unwrap()).unwrap();
        assert_eq!(frame, expected);
        assert_eq!((frame.width, frame.height), (4, 6));
        assert_eq!(s.undo_depth(), depth);
        s.statistics_need_read_ahead(true);
        assert!(s.run("raster.imageGrid", &json!({"layer":id})).unwrap_err().contains("background"));
        let ahead = crate::cmd::reads::read_ahead(&mut s, "raster.imageGrid", &json!({"layer":id})).unwrap();
        let done = ahead.run(&rustgis_analysis::control::Control::new()).unwrap();
        assert_eq!(serde_json::from_value::<Frame>(crate::cmd::reads::read_land(&mut s, done).unwrap()).unwrap(), expected);
    }
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn annotation_grid_file_changes_before_selection_or_queued_read_require_a_pixel_reload() {
        for queued in [false, true] {
            let (mut s, id, original) = session();
            let folder = std::env::temp_dir().join(format!("rustgis-annotation-grid-file-{}-{queued}", std::process::id()));
            std::fs::create_dir_all(&folder).unwrap();
            let path = folder.join("image.tif");
            let bytes = s.map().layer(id).unwrap().raster.as_ref().unwrap().embedded.clone().unwrap();
            std::fs::write(&path, bytes.as_slice()).unwrap();
            let pixels = rustgis_data::raster::read_path(&path).unwrap();
            s.map_mut().layer_mut(id).unwrap().raster = Some(RasterLayer::from_path(pixels, path.display().to_string()));
            let displayed = s.map().layer(id).unwrap().raster.as_ref().unwrap().data.clone().unwrap();
            assert_eq!(serde_json::from_value::<Frame>(s.run("raster.imageGrid", &json!({"layer":id})).unwrap()).unwrap(), original);
            let ahead = queued.then(|| crate::cmd::reads::read_ahead(&mut s, "raster.imageGrid", &json!({"layer":id})).unwrap());
            let replacement = Raster::new(
                "new",
                7,
                3,
                vec![vec![2.; 21]],
                GeoTransform { origin_x: 5000., origin_y: 9000., pixel_width: 10., pixel_height: -10. },
                Crs::from_epsg(3857).unwrap(),
                None,
                ColorHint::Gray,
                "UInt8",
            )
            .unwrap();
            let replacement = geotiff::write(&replacement, 0).unwrap();
            std::fs::write(&path, &replacement).unwrap();
            let depth = s.undo_depth();
            let result = if let Some(ahead) = ahead {
                let done = ahead.run(&rustgis_analysis::control::Control::new()).unwrap();
                crate::cmd::reads::read_land(&mut s, done)
            } else {
                s.run("raster.imageGrid", &json!({"layer":id}))
            };
            assert!(result.unwrap_err().contains("Reload Data"));
            assert_eq!(s.undo_depth(), depth);
            assert!(Arc::ptr_eq(&displayed, s.map().layer(id).unwrap().raster.as_ref().unwrap().data.as_ref().unwrap()));
            s.run("layer.reloadData", &json!({"layer":id})).unwrap();
            let expected = Frame::from_grid(&geotiff::read_image_grid(std::io::Cursor::new(&replacement)).unwrap()).unwrap();
            let frame: Frame = serde_json::from_value(s.run("raster.imageGrid", &json!({"layer":id})).unwrap()).unwrap();
            assert_eq!(frame, expected);
            assert_ne!(frame, original);
            std::fs::remove_dir_all(folder).unwrap();
        }
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn annotation_grid_converted_sources_offer_geotiff_recovery_without_a_reload_loop() {
        struct Dir(std::path::PathBuf);
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let dir = Dir(std::env::temp_dir().join(format!("rustgis-grid-converted-{}", std::process::id())));
        std::fs::create_dir_all(&dir.0).unwrap();
        let (mut s, id, expected) = session();
        let cache = dir.0.join("converted.tif");
        std::fs::write(&cache, s.map().layer(id).unwrap().raster.as_ref().unwrap().embedded.as_ref().unwrap().as_slice()).unwrap();
        let converted =
            rustgis_data::raster::gdal::Converted { path: cache.clone(), subdataset: None, others: Vec::new(), assumed_lonlat: false };
        for extension in ["nc", "jp2"] {
            let source = dir.0.join(format!("original.{extension}"));
            std::fs::write(&source, b"original source represented by the conversion result").unwrap();
            for queued in [false, true] {
                // Model the result of GDAL conversion, without needing GDAL installed for this test.
                for _ in 0..2 {
                    let pixels = rustgis_data::raster::read_converted(&source, &converted).unwrap();
                    s.map_mut().layer_mut(id).unwrap().raster = Some(RasterLayer::from_path(pixels, source.display().to_string()));
                    let depth = s.undo_depth();
                    let params = json!({"layer":id});
                    let result = if queued {
                        let read = crate::cmd::reads::read_ahead(&mut s, "raster.imageGrid", &params).unwrap();
                        let done = read.run(&rustgis_analysis::control::Control::new()).unwrap();
                        crate::cmd::reads::read_land(&mut s, done)
                    } else {
                        s.run("raster.imageGrid", &params)
                    };
                    let error = result.unwrap_err();
                    assert!(error.contains("GeoTIFF") && error.contains("Export") && error.contains("add"), "{error}");
                    assert!(!error.contains("changed") && !error.contains("Reload Data"), "{error}");
                    assert_eq!(s.undo_depth(), depth);
                }
            }
        }
        // Adding the actual GeoTIFF supplies a verified original grid.
        let pixels = rustgis_data::raster::read_path(&cache).unwrap();
        s.map_mut().layer_mut(id).unwrap().raster = Some(RasterLayer::from_path(pixels, cache.display().to_string()));
        assert_eq!(serde_json::from_value::<Frame>(s.run("raster.imageGrid", &json!({"layer":id})).unwrap()).unwrap(), expected);
    }

    #[test]
    fn annotation_grid_background_read_cancels_and_rejects_replaced_sources() {
        let (mut s, id, _) = session();
        let ahead = crate::cmd::reads::read_ahead(&mut s, "raster.imageGrid", &json!({"layer":id})).unwrap();
        let control = rustgis_analysis::control::Control::new();
        control.cancel();
        assert!(ahead.run(&control).is_err());
        let ahead = crate::cmd::reads::read_ahead(&mut s, "raster.imageGrid", &json!({"layer":id})).unwrap();
        let done = ahead.run(&rustgis_analysis::control::Control::new()).unwrap();
        s.map_mut().layer_mut(id).unwrap().raster.as_mut().unwrap().embedded = Some(Arc::new(vec![0; 32]));
        assert!(crate::cmd::reads::read_land(&mut s, done).is_err());
    }
    #[test]
    fn annotation_grid_scoped_reads_preserve_approved_selectors_and_refuse_retargeting() {
        use crate::policy::{Call, Outcome, Scope};
        struct Live;
        impl crate::policy::Revocation for Live {
            fn allows(&self, _: crate::policy::Effects) -> bool {
                true
            }
        }
        for named in [false, true] {
            for retarget in [false, true] {
                let (mut s, id, expected) = session();
                s.active_layer = Some(id);
                let params = if named { json!({"layer":"Image"}) } else { json!({}) };
                let call = Call::command("raster.imageGrid", params);
                let scope = Scope::assistant(1717, Arc::new(Live));
                let Outcome::NeedsApproval(pending) = s.review(&scope, &call) else { panic!("image reads require Files approval") };
                let read = s.start_scoped_archive_read(scope, &call, Some(pending.allow())).unwrap();
                let done = read.run(&rustgis_analysis::control::Control::new()).unwrap();
                if retarget {
                    s.run("layer.rename", &json!({"layer":id,"name":"Old image"})).unwrap();
                    let raster = s.map().layer(id).unwrap().raster.clone().unwrap();
                    let other = s.map_mut().add_raster("Image", raster);
                    s.active_layer = Some(other);
                }
                let answer = crate::cmd::reads::read_land(&mut s, done);
                if retarget {
                    assert!(answer.is_err());
                } else {
                    assert_eq!(serde_json::from_value::<Frame>(answer.unwrap()).unwrap(), expected);
                }
            }
        }
    }
}
