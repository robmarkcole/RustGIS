//! Single-image annotation metadata carried by GeoJSON's `rustgis_annotation` foreign member.
//! Categories reuse COCO names/IDs and its panoptic RGB colour convention. Pixel boxes use
//! four corners, as in https://docs.ultralytics.com/datasets/obb/; no model angle convention.
use crate::raster::{Crs, GeoTransform};
use serde::{Deserialize, Serialize};

/// The same metadata limit is enforced on import and export.
pub const MAX_BYTES: usize = 1 << 20;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Image {
    pub file_name: String,
    pub width: u32,
    pub height: u32,
    pub crs: Crs,
    /// GDAL order: x origin, x/column, x/row, y origin, y/column, y/row.
    pub affine: [f64; 6],
}
impl Image {
    pub fn validate(&self) -> Result<(), String> {
        let [_, a, b, _, c, d] = self.affine;
        if self.file_name.is_empty()
            || self.file_name.len() > 1024
            || self.file_name.contains(['/', '\\', '\0', ':'])
            || matches!(self.file_name.as_str(), "." | "..")
            || self.width == 0
            || self.height == 0
            || !self.affine.iter().all(|v| v.is_finite())
            || !(a * d - b * c).is_normal()
            || self.crs.lacks_datum_shift()
            || self.crs.transformer().is_err()
        {
            return Err("annotation image needs a filename, positive dimensions and an invertible supported pixel grid".into());
        }
        Ok(())
    }
    pub fn transform(&self) -> GeoTransform {
        let [x, a, _, y, _, d] = self.affine;
        GeoTransform { origin_x: x, origin_y: y, pixel_width: a, pixel_height: d }
    }
    pub fn same_grid(&self, other: &Self) -> bool {
        self.width == other.width && self.height == other.height && self.crs == other.crs && self.affine == other.affine
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Category {
    pub id: u32,
    pub name: String,
    pub color: [u8; 3],
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Metadata {
    pub version: u32,
    pub image: Image,
    pub categories: Vec<Category>,
}
impl Metadata {
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1 {
            return Err("unsupported image annotation metadata version; update RustGIS".into());
        }
        self.image.validate()?;
        if self.categories.len() > 64 {
            return Err("at most 64 annotation categories are supported".into());
        }
        let mut ids = std::collections::BTreeSet::new();
        let mut names = std::collections::BTreeSet::new();
        for c in &self.categories {
            if c.name.trim().is_empty() || c.name.chars().count() > 200 || !ids.insert(c.id) || !names.insert(&c.name) {
                return Err("annotation categories need unique IDs and nonempty unique names of at most 200 characters".into());
            }
        }
        if serde_json::to_vec(self).map_err(|e| e.to_string())?.len() > MAX_BYTES {
            return Err("image annotation metadata exceeds 1 MiB".into());
        }
        Ok(())
    }
}

/// Derived exchange fields, never the authority for constrained editing. Four ordered pixel
/// corners avoid the angle/width conventions of model-specific OBB formats.
pub(crate) fn decorate(props: &mut serde_json::Map<String, serde_json::Value>, meta: &Metadata) {
    use serde_json::{Value, json};
    if let Some(c) = props.get("class").and_then(Value::as_str).and_then(|n| meta.categories.iter().find(|c| c.name == n)) {
        props.insert("category_id".into(), json!(c.id));
    }
    // Never carry a stale externally supplied OBB when the box construction was removed.
    props.remove("obb");
    let corners = (|| {
        let raw = props.get("_rustgis_box")?.as_str()?;
        if raw.len() > 16_384 {
            return None;
        }
        let b: Value = serde_json::from_str(raw).ok()?;
        if b.get("version")?.as_u64()? != 1 {
            return None;
        }
        let f = b.get("frame")?;
        let affine: [f64; 6] = serde_json::from_value(f.get("affine")?.clone()).ok()?;
        let crs: Crs = serde_json::from_value(f.get("crs")?.clone()).ok()?;
        if affine != meta.image.affine || crs != meta.image.crs {
            return None;
        }
        let center: [f64; 2] = serde_json::from_value(b.get("center")?.clone()).ok()?;
        let w = b.get("width")?.as_f64()?;
        let h = b.get("height")?.as_f64()?;
        let angle = b.get("angle")?.as_f64()?;
        if !center.iter().chain([&w, &h, &angle]).all(|v| v.is_finite()) || w <= 0. || h <= 0. {
            return None;
        }
        let (sn, cs) = angle.to_radians().sin_cos();
        Some(
            [[-w / 2., -h / 2.], [w / 2., -h / 2.], [w / 2., h / 2.], [-w / 2., h / 2.]]
                .map(|[x, y]| [center[0] + cs * x - sn * y, center[1] + sn * x + cs * y]),
        )
    })();
    if let Some(corners) = corners {
        props.insert("obb".into(), json!(corners));
    }
}

/// Saved datasets must refuse future or invalid metadata just as GeoJSON readers do.
pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Metadata>, D::Error> {
    let meta = Option::<Metadata>::deserialize(d)?;
    if let Some(m) = &meta {
        m.validate().map_err(serde::de::Error::custom)?;
    }
    Ok(meta)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn annotation_palette_roundtrips_escaped_names_with_one_shared_size_limit() {
        let meta = Metadata {
            version: 1,
            image: Image { file_name: "tile.tif".into(), width: 32, height: 32, crs: Crs::WebMercator, affine: [0., 1., 0., 0., 0., -1.] },
            categories: (0..64).map(|id| Category { id, name: format!("{}{id}", "\0".repeat(180)), color: [1, 2, 3] }).collect(),
        };
        meta.validate().unwrap();
        let mut ds = crate::Dataset::new("Empty", crate::GeometryType::Polygon, vec![]);
        ds.annotation = Some(meta.clone());
        let (bytes, _) = crate::io::write_bytes(&ds, "geojson").unwrap();
        assert!(bytes.len() > 65_536);
        let (back, _) = crate::io::read_bytes("tile.geojson", &bytes).unwrap();
        assert_eq!(back.annotation, Some(meta));
        assert!(back.features.is_empty());
    }
}
