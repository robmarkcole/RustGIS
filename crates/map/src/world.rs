//! World map projections the map draws in: Robinson, Equal Earth, Natural Earth, Winkel Tripel
//! and Eckert IV, on a sphere of the WGS 84 equatorial radius unless the definition gives `+R`.
//! They are computed in `rustgis_geom::world`, where a file's coordinate system in one of them is
//! read with the same formulas (opengeos/RustGIS#1701).

pub use rustgis_geom::world::{Kind, World};
