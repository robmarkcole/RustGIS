//! The original image grid, independent of the pixels read for display.

use super::Crs;
use crate::{DataError, Result};
use rustgis_geom::Coord;

/// An image's dimensions and full affine placement in its native coordinate system.
///
/// Unlike [`super::Raster`], this describes the original image, not a north-up resampling or
/// overview. Pixel positions refer to corners at whole numbers and centres at +0.5, including
/// for a GeoTIFF whose samples are PixelIsPoint. Coordinates outside the image are not clipped.
/// This is a runtime value, not a new saved-file representation.
#[derive(Clone, Debug)]
pub struct ImageGrid {
    width: u32,
    height: u32,
    crs: Crs,
    affine: [f64; 6],
    inverse: [f64; 4],
}

impl ImageGrid {
    /// A positive-sized grid placed by `[x_origin, x_column, x_row, y_origin, y_column, y_row]`.
    /// Rejects non-finite, singular or numerically unrepresentable placements.
    pub fn new(width: u32, height: u32, crs: Crs, affine: [f64; 6]) -> Result<Self> {
        let bad = || DataError::Format("image grid needs positive dimensions and a finite, invertible affine transform".into());
        if width == 0 || height == 0 || !affine.iter().all(|v| v.is_finite()) {
            return Err(bad());
        }
        let [_, a, b, _, c, d] = affine;
        // Normalise before taking the determinant: valid very small/large pixel sizes must not
        // underflow/overflow merely because their product is outside f64's range.
        let scale = a.abs().max(b.abs()).max(c.abs()).max(d.abs());
        if scale == 0.0 {
            return Err(bad());
        }
        let (a, b, c, d) = (a / scale, b / scale, c / scale, d / scale);
        let det = a * d - b * c;
        if det == 0.0 {
            return Err(bad());
        }
        let inverse = [d / det / scale, -b / det / scale, -c / det / scale, a / det / scale];
        if !inverse.iter().all(|v| v.is_finite()) {
            return Err(bad());
        }
        let grid = Self { width, height, crs, affine, inverse };
        for (col, row) in [(0., 0.), (width as f64, 0.), (width as f64, height as f64), (0., height as f64)] {
            if grid.to_world(col, row).is_none() {
                return Err(bad());
            }
        }
        Ok(grid)
    }

    /// Original image width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Original image height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// The native coordinate system. Use [`Crs::transformer`] to convert world coordinates to
    /// WGS 84 longitude/latitude, subject to that transformer's projection/datum limitations.
    pub fn crs(&self) -> Crs {
        self.crs
    }

    /// Full affine transform, in the order accepted by [`Self::new`].
    pub fn affine(&self) -> [f64; 6] {
        self.affine
    }

    /// Pixel position to native coordinates: `x = x_origin + col*x_column + row*x_row`, and
    /// likewise for y. Returns `None` for non-finite input or an overflowing result.
    pub fn to_world(&self, col: f64, row: f64) -> Option<Coord> {
        if !col.is_finite() || !row.is_finite() {
            return None;
        }
        let [x, a, b, y, c, d] = self.affine;
        let out = Coord::new(x + a * col + b * row, y + c * col + d * row);
        out.is_finite().then_some(out)
    }

    /// Native coordinates to a fractional pixel position, without rounding or clipping.
    /// Returns `None` for non-finite input or an overflowing result.
    pub fn to_pixel(&self, world: Coord) -> Option<(f64, f64)> {
        if !world.is_finite() {
            return None;
        }
        let (x, y) = (world.x - self.affine[0], world.y - self.affine[3]);
        let [a, b, c, d] = self.inverse;
        let out = (a * x + b * y, c * x + d * y);
        (out.0.is_finite() && out.1.is_finite()).then_some(out)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::raster::geotiff;
    use std::io::{Cursor, Read, Seek, SeekFrom};
    use tiff::decoder::Decoder;
    use tiff::encoder::{TiffEncoder, colortype};
    use tiff::tags::Tag;

    const KEYS: [u16; 16] = [1, 1, 0, 3, 1024, 0, 1, 1, 1025, 0, 1, 1, 3072, 0, 1, 32633];

    /// A full-resolution image, optionally followed by a smaller overview, with chosen geo tags.
    fn tiff(matrix: Option<&[f64]>, scale: Option<&[f64]>, ties: Option<&[f64]>, keys: Option<&[u16]>, overview: bool) -> Vec<u8> {
        tiff_with_params(matrix, scale, ties, keys, None, None, overview)
    }

    fn tiff_with_params(
        matrix: Option<&[f64]>,
        scale: Option<&[f64]>,
        ties: Option<&[f64]>,
        keys: Option<&[u16]>,
        parameters: Option<&[f64]>,
        ascii: Option<&str>,
        overview: bool,
    ) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        {
            let mut enc = TiffEncoder::new(&mut out).unwrap();
            let mut image = enc.new_image::<colortype::Gray8>(20, 10).unwrap();
            for (tag, values) in [(34264, matrix), (33550, scale), (33922, ties)] {
                if let Some(values) = values {
                    image.encoder().write_tag(Tag::Unknown(tag), values).unwrap();
                }
            }
            if let Some(keys) = keys {
                image.encoder().write_tag(Tag::GeoKeyDirectoryTag, keys).unwrap();
            }
            if let Some(parameters) = parameters {
                image.encoder().write_tag(Tag::Unknown(34736), parameters).unwrap();
            }
            if let Some(ascii) = ascii {
                image.encoder().write_tag(Tag::Unknown(34737), ascii).unwrap();
            }
            image.write_data(&[7; 200]).unwrap();
            if overview {
                let mut image = enc.new_image::<colortype::Gray8>(5, 5).unwrap();
                image.encoder().write_tag(Tag::NewSubfileType, 1u32).unwrap();
                image.write_data(&[9; 25]).unwrap();
            }
        }
        out.into_inner()
    }

    /// Fails any attempt to read pixel bytes, while allowing TIFF directory/tag reads.
    struct NoPixels {
        input: Cursor<Vec<u8>>,
        strips: Vec<std::ops::Range<u64>>,
    }

    impl NoPixels {
        fn new(bytes: Vec<u8>) -> Self {
            let mut dec = Decoder::new(Cursor::new(&bytes)).unwrap();
            let mut strips = Vec::new();
            loop {
                let offsets = dec.get_tag_u64_vec(Tag::StripOffsets).unwrap();
                let counts = dec.get_tag_u64_vec(Tag::StripByteCounts).unwrap();
                strips.extend(offsets.into_iter().zip(counts).map(|(a, n)| a..a + n));
                if !dec.more_images() {
                    break;
                }
                dec.next_image().unwrap();
            }
            Self { input: Cursor::new(bytes), strips }
        }
    }

    impl Read for NoPixels {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            let start = self.input.position();
            if self.strips.iter().any(|r| start < r.end && start + bytes.len() as u64 > r.start) {
                return Err(std::io::Error::other("attempted to read pixel data"));
            }
            self.input.read(bytes)
        }
    }

    impl Seek for NoPixels {
        fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
            self.input.seek(pos)
        }
    }

    #[test]
    fn image_grid_reads_original_header_without_pixels_or_an_overview() {
        let bytes = tiff(None, Some(&[2., 3., 0.]), Some(&[4., 5., 0., 500_008., 985., 0.]), Some(&KEYS), true);
        let grid = geotiff::read_image_grid(NoPixels::new(bytes.clone())).unwrap();
        assert_eq!((grid.width(), grid.height(), grid.crs()), (20, 10, Crs::from_epsg(32633).unwrap()));
        assert_eq!(grid.affine(), [500_000., 2., 0., 1000., 0., -3.]);
        // The ordinary reader deliberately chooses an overview when display memory is limited.
        let displayed = geotiff::read_limited(geotiff::reader(Cursor::new(bytes)).unwrap(), "overview", 25, None).unwrap();
        assert_eq!((displayed.width, displayed.height), (5, 5));
        assert!(displayed.overview.is_some());
        assert_ne!(displayed.transform.pixel_width, grid.affine()[1]);
    }

    #[test]
    fn image_grid_preserves_rotation_and_pixel_is_point_corner_convention() {
        let matrix = [0., -3., 0., 500_000., 2., 0., 0., 1000., 0., 0., 0., 0., 0., 0., 0., 1.];
        for point in [false, true] {
            let bytes = geotiff::tests_support::matrix_tiff(matrix, point);
            let grid = geotiff::read_image_grid(NoPixels::new(bytes.clone())).unwrap();
            let (x, y) = if point { (500_001.5, 999.) } else { (500_000., 1000.) };
            assert_eq!(grid.affine(), [x, 0., -3., y, 2., 0.]);
            assert_eq!((grid.width(), grid.height()), (20, 10));
            if point {
                assert_eq!(grid.to_world(0.5, 0.5), Some(Coord::new(500_000., 1000.)));
            }
            let displayed = geotiff::read(Cursor::new(bytes), "rotated").unwrap();
            assert_ne!((displayed.width, displayed.height), (grid.width(), grid.height()));
        }
        let mut keys = KEYS;
        keys[11] = 2;
        let bytes = tiff(None, Some(&[2., 3., 0.]), Some(&[4., 5., 0., 500_008., 985., 0.]), Some(&keys), false);
        let grid = geotiff::read_image_grid(NoPixels::new(bytes)).unwrap();
        assert_eq!(grid.affine(), [499_999., 2., 0., 1001.5, 0., -3.]);
        assert_eq!(grid.to_world(4.5, 5.5), Some(Coord::new(500_008., 985.)));
    }

    #[test]
    fn image_grid_reads_bigtiff_and_refuses_an_overview_or_mask_as_the_original() {
        for kind in [0, 1, 4] {
            let mut out = Cursor::new(Vec::new());
            {
                let mut enc = TiffEncoder::new_big(&mut out).unwrap();
                let mut image = enc.new_image::<colortype::Gray8>(20, 10).unwrap();
                image.encoder().write_tag(Tag::NewSubfileType, kind as u32).unwrap();
                image.encoder().write_tag(Tag::GeoKeyDirectoryTag, &KEYS[..]).unwrap();
                image.encoder().write_tag(Tag::Unknown(33550), &[2., 3., 0.][..]).unwrap();
                image.encoder().write_tag(Tag::Unknown(33922), &[0., 0., 0., 500_000., 1000., 0.][..]).unwrap();
                image.write_data(&[7; 200]).unwrap();
            }
            let result = geotiff::read_image_grid(NoPixels::new(out.into_inner()));
            if kind == 0 {
                assert_eq!(result.unwrap().affine(), [500_000., 2., 0., 1000., 0., -3.]);
            } else {
                assert!(result.unwrap_err().to_string().contains("overview or mask"));
            }
        }
    }

    #[test]
    fn image_grid_stays_strict_while_ordinary_reads_keep_complete_geokeys() {
        for count in [4, u16::MAX] {
            for partial_tail in [false, true] {
                let mut keys = KEYS.to_vec();
                keys[3] = count;
                if partial_tail {
                    keys.extend([2048, 0]); // An incomplete entry must not hide the complete projected CRS.
                }
                // Exercise both an unknown-CRS error and a silent lon/lat fallback without the fix.
                for (x, y) in [(500_000., 1000.), (10., 50.)] {
                    let bytes = tiff(None, Some(&[1., 1., 0.]), Some(&[0., 0., 0., x, y, 0.]), Some(&keys), false);
                    let raster = geotiff::read(Cursor::new(&bytes), "truncated keys").unwrap();
                    assert_eq!(raster.crs, Crs::from_epsg(32633).unwrap());
                    assert!(raster.georeferenced && raster.lost.is_empty());
                    let error = geotiff::read_image_grid(Cursor::new(bytes)).unwrap_err();
                    assert!(error.to_string().contains("incomplete GeoKey directory"));
                }
            }
        }
    }

    #[test]
    fn image_grid_bounds_keys_without_changing_ordinary_reads_with_short_counts() {
        for count in [0, 1, 2] {
            let mut keys = KEYS;
            keys[3] = count;
            keys[11] = 2; // PixelIsPoint, including when it lies past the declared count.
            for (x, y) in [(500_000., 1000.), (10., 50.)] {
                let bytes = tiff(None, Some(&[1., 1., 0.]), Some(&[0., 0., 0., x, y, 0.]), Some(&keys), false);
                let raster = geotiff::read(Cursor::new(&bytes), "short count").unwrap();
                assert_eq!(raster.crs, Crs::from_epsg(32633).unwrap());
                assert_eq!((raster.transform.origin_x, raster.transform.origin_y), (x - 0.5, y + 0.5));
                assert!(raster.georeferenced && raster.lost.is_empty());
                assert!(geotiff::read_image_grid(Cursor::new(bytes)).is_err());
            }
        }
        // A trailing SHORT parameter that resembles a raster-type key must not shift an exact grid.
        let keys = [1, 1, 0, 1, 3072, 0, 1, 32633, 1025, 0, 1, 2];
        let bytes = tiff(None, Some(&[1., 1., 0.]), Some(&[0., 0., 0., 10., 50., 0.]), Some(&keys), false);
        let grid = geotiff::read_image_grid(Cursor::new(bytes)).unwrap();
        assert_eq!(grid.affine(), [10., 1., 0., 50., 0., -1.]);
    }

    #[test]
    fn image_grid_refuses_dangling_or_mistyped_custom_crs_parameters() {
        // Custom Transverse Mercator, with a DOUBLE central meridian and ASCII citation.
        let keys = [
            1, 1, 0, 7, 1024, 0, 1, 1, 1025, 0, 1, 1, 2048, 0, 1, 4326, 3072, 0, 1, 32767, 3075, 0, 1, 1, 3080, 34736, 1, 0, 3073, 34737,
            8, 0,
        ];
        let read = |keys: &[u16], parameters: Option<&[f64]>, ascii: Option<&str>| {
            let bytes =
                tiff_with_params(None, Some(&[1., 1., 0.]), Some(&[0., 0., 0., 500_000., 1000., 0.]), Some(keys), parameters, ascii, false);
            geotiff::read_image_grid(NoPixels::new(bytes))
        };
        let grid = read(&keys, Some(&[15.]), Some("Test TM|")).unwrap();
        let definition = grid.crs().custom_def().unwrap();
        assert!(definition.proj4.contains("+proj=tmerc") && definition.proj4.contains("+lon_0=15"));
        assert_eq!(definition.label, "Test TM");
        let origin = grid.crs().transformer().unwrap().to_lonlat(Coord::new(0., 0.)).unwrap();
        assert!((origin.x - 15.).abs() < 1e-9 && origin.y.abs() < 1e-9);
        for parameters in [None, Some(&[][..]), Some(&[f64::NAN][..]), Some(&[f64::INFINITY][..])] {
            assert!(read(&keys, parameters, Some("Test TM|")).unwrap_err().to_string().contains("parameter reference"));
        }
        for (index, value) in [
            (25, 0),
            (25, 34735),
            (25, 34737),
            (25, 65000), // Unsupported or wrong storage for a numeric key.
            (26, 0),
            (26, 2),
            (27, 1),
            (27, u16::MAX), // Zero/multiple values and out-of-range DOUBLE offsets.
            (29, 0),
            (29, 34736),
            (30, 0),
            (30, 9),
            (31, 1),
            (31, u16::MAX), // Invalid ASCII references.
            (21, 34736),    // A projection-method code must remain an inline SHORT.
        ] {
            let mut bad = keys;
            bad[index] = value;
            assert!(
                read(&bad, Some(&[15.]), Some("Test TM|")).unwrap_err().to_string().contains("parameter reference"),
                "{index}: {value}"
            );
        }
        assert!(read(&keys, Some(&[15.]), None).unwrap_err().to_string().contains("parameter reference"));
    }

    #[test]
    fn image_grid_refuses_missing_unknown_or_malformed_georeferencing() {
        let read = |bytes| geotiff::read_image_grid(Cursor::new(bytes));
        assert!(read(tiff(None, None, None, None, false)).unwrap_err().to_string().contains("embedded"));
        assert!(read(tiff(None, Some(&[1., 1., 0.]), Some(&[0.; 6]), None, false)).is_err());
        for (index, value) in [(0, 2), (3, 4), (7, 3), (10, 2), (11, 3)] {
            let mut keys = KEYS;
            keys[index] = value;
            assert!(read(tiff(None, Some(&[1., 1., 0.]), Some(&[0.; 6]), Some(&keys), false)).is_err());
        }
        // Unreferenced SHORT parameter values in the directory tail are not extra GeoKeys.
        let mut keys = KEYS;
        keys[3] = 2;
        assert!(read(tiff(None, Some(&[1., 1., 0.]), Some(&[0.; 6]), Some(&keys), false)).is_err());
        let mut unknown = KEYS;
        unknown[15] = 65534;
        assert!(read(tiff(None, Some(&[1., 1., 0.]), Some(&[0.; 6]), Some(&unknown), false)).unwrap_err().to_string().contains("65534"));
        // A partial transformation matrix must not fall back to an otherwise valid scale/tie.
        assert!(read(tiff(Some(&[1.; 8]), Some(&[1., 1., 0.]), Some(&[0.; 6]), Some(&KEYS), false)).is_err());
        for matrix in [
            [0.; 16],
            [1., 2., 0., 0., 2., 4., 0., 0., 0., 0., 0., 0., 0., 0., 0., 1.],
            [1., 0., 0., f64::NAN, 0., -1., 0., 0., 0., 0., 0., 0., 0., 0., 0., 1.],
        ] {
            assert!(read(tiff(Some(&matrix), None, None, Some(&KEYS), false)).is_err());
        }
        for (scale, ties) in [
            (vec![1., 1.], vec![0.; 6]),
            (vec![0., 0., 0.], vec![0.; 6]),
            (vec![1., 1., 0.], vec![0.; 5]),
            (vec![1., f64::INFINITY, 0.], vec![0.; 6]),
            (vec![1., 1., 0.], vec![0., 0., 0., 0., 0., 0., 1., 1., 0., 30., 40., 0.]),
        ] {
            assert!(read(tiff(None, Some(&scale), Some(&ties), Some(&KEYS), false)).is_err());
        }
        let ties = [0., 0., 0., 100., 200., 0., 1., 1., 0., 102., 197., 0.];
        assert!(read(tiff(None, Some(&[2., 3., 0.]), Some(&ties), Some(&KEYS), false)).is_ok());
        // The metadata limit applies even though no pixel buffer is allocated.
        assert!(read(tiff(Some(&vec![0.; (1 << 17) + 1]), None, None, Some(&KEYS), false)).is_err());
    }

    #[test]
    fn image_grid_handles_truncated_headers_and_honours_cancellation() {
        let bytes = geotiff::tests_support::strip_tiff();
        for n in 0..bytes.len() {
            let _ = geotiff::read_image_grid(Cursor::new(&bytes[..n]));
        }
        let probe = crate::progress::tests::Counted::cancelling_after(0);
        let _guard = crate::progress::install(Some(probe));
        assert!(matches!(geotiff::read_image_grid(Cursor::new(bytes)), Err(DataError::Cancelled)));
    }

    #[test]
    fn image_grid_affine_round_trips_corners_centres_and_outside_positions() {
        for affine in [
            [100., 2., 0., 200., 0., -3.],
            [100., 0., -3., 200., 2., 0.],
            [100., 2., 1., 200., 0.5, -3.],
            [100., -2., 0.5, 200., 1., 3.],
            [0., 1e-200, 0., 0., 0., -2e-200],
            [0., 1e200, 0., 0., 0., -2e200],
        ] {
            let grid = ImageGrid::new(20, 10, Crs::WebMercator, affine).unwrap();
            for (col, row) in [(0., 0.), (0.5, 0.5), (20., 10.), (7.25, 3.75), (-2., 12.)] {
                let world = grid.to_world(col, row).unwrap();
                let back = grid.to_pixel(world).unwrap();
                assert!((back.0 - col).abs() < 1e-10 && (back.1 - row).abs() < 1e-10, "{affine:?}: {back:?}");
            }
        }
        let grid = ImageGrid::new(20, 10, Crs::LonLat, [100., 2., 1., 200., 0.5, -3.]).unwrap();
        assert_eq!(grid.to_world(4., 5.), Some(Coord::new(113., 187.)));
    }

    #[test]
    fn image_grid_rejects_invalid_placements_and_nonfinite_conversions() {
        for affine in [
            [0.; 6],
            [0., 1., 2., 0., 2., 4.],
            [f64::NAN, 1., 0., 0., 0., -1.],
            [0., f64::INFINITY, 0., 0., 0., -1.],
            [0., 1e-320, 0., 0., 0., -1e-320],
            [f64::MAX, f64::MAX, 0., 0., 0., -1.],
        ] {
            assert!(ImageGrid::new(20, 10, Crs::LonLat, affine).is_err());
        }
        let affine = [0., 1., 0., 0., 0., -1.];
        assert!(ImageGrid::new(0, 10, Crs::LonLat, affine).is_err());
        assert!(ImageGrid::new(20, 0, Crs::LonLat, affine).is_err());
        let grid = ImageGrid::new(20, 10, Crs::LonLat, affine).unwrap();
        assert!(grid.to_world(f64::NAN, 0.).is_none());
        assert!(grid.to_world(0., f64::INFINITY).is_none());
        assert!(grid.to_pixel(Coord::new(f64::INFINITY, 0.)).is_none());
    }

    #[test]
    fn image_grid_composes_with_the_existing_crs_transformer() {
        let grid = ImageGrid::new(20, 10, Crs::from_epsg(32633).unwrap(), [500_000., 2., 1., 0., 0.5, -3.]).unwrap();
        let t = grid.crs().transformer().unwrap();
        let origin = t.to_lonlat(grid.to_world(0., 0.).unwrap()).unwrap();
        assert!((origin.x - 15.).abs() < 1e-9 && origin.y.abs() < 1e-9);
        let ll = t.to_lonlat(grid.to_world(7.25, 3.75).unwrap()).unwrap();
        let back = grid.to_pixel(t.from_lonlat(ll).unwrap()).unwrap();
        assert!((back.0 - 7.25).abs() < 1e-7 && (back.1 - 3.75).abs() < 1e-7);
    }
}
