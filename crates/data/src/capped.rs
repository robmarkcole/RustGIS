//! A file made in memory under a size limit, such as a download in the browser: its writer fails
//! as soon as the file would pass the limit, so it is never held larger.

use std::io::{Error, ErrorKind, Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::{DataError, Result};

/// The most bytes the buffers a file is made in may hold in all (a Shapefile's parts share one).
#[derive(Debug)]
pub struct Limit {
    max: usize,
    held: AtomicUsize,
    passed: AtomicBool,
}

impl Limit {
    /// A limit of `max` bytes.
    pub const fn new(max: usize) -> Self {
        Limit { max, held: AtomicUsize::new(0), passed: AtomicBool::new(false) }
    }

    /// No limit but memory: for a file that is to be written.
    pub const fn none() -> Self {
        Limit::new(usize::MAX)
    }

    /// Whether a write was refused for passing the limit.
    pub fn passed(&self) -> bool {
        self.passed.load(Ordering::Relaxed)
    }

    /// An empty buffer counted against this limit.
    pub(crate) fn buffer(&self) -> Buffer<'_> {
        Buffer { file: Vec::new(), at: 0, limit: self, charged: 0 }
    }

    /// Counts `bytes` held apart from the buffers (files made in them and taken out, while an
    /// archive of them is made) against the limit until the guard is dropped; refused past it.
    pub(crate) fn hold(&self, bytes: usize) -> Result<Held<'_>> {
        self.take(bytes).map_err(|_| DataError::Format(format!("the file passes the {} bytes it may come to", self.max)))?;
        Ok(Held { limit: self, bytes })
    }

    /// `made`, or an error once a write was refused: a writer may drop that write's error (some
    /// finish their file as they are dropped), so this, not its result, says whether the file is
    /// whole.
    pub(crate) fn whole<T>(&self, made: Result<T>) -> Result<T> {
        if self.passed() {
            return Err(DataError::Format(format!("the file passes the {} bytes it may come to", self.max)));
        }
        made
    }

    /// Counts `n` more bytes held, refusing them past the limit, and gives how many are held.
    fn take(&self, n: usize) -> std::io::Result<usize> {
        // In one update, so the count never holds a total past the limit, even for a moment
        // another buffer on the same limit could see.
        let grown = |held: usize| held.checked_add(n).filter(|total| *total <= self.max);
        match self.held.try_update(Ordering::Relaxed, Ordering::Relaxed, grown) {
            Ok(before) => Ok(before + n),
            Err(_) => {
                self.passed.store(true, Ordering::Relaxed);
                Err(Error::other("the file passes the size it may come to"))
            }
        }
    }
}

/// Bytes counted against a [`Limit`] while they are held ([`Limit::hold`]).
pub(crate) struct Held<'a> {
    limit: &'a Limit,
    bytes: usize,
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        self.limit.held.fetch_sub(self.bytes, Ordering::Relaxed);
    }
}

/// A file made in memory within a [`Limit`], written and sought as a file is.
#[derive(Debug)]
pub(crate) struct Buffer<'a> {
    file: Vec<u8>,
    /// Where the next write goes.
    at: u64,
    limit: &'a Limit,
    /// The allocation counted against the limit: what the system holds, not the file's length.
    charged: usize,
}

impl Buffer<'_> {
    /// The file made, its spare room given back, no longer counted against the limit.
    pub(crate) fn into_inner(mut self) -> Vec<u8> {
        let mut file = std::mem::take(&mut self.file);
        file.shrink_to_fit();
        self.release();
        file
    }

    /// Stops counting the buffer's allocation against the limit, once.
    fn release(&mut self) {
        self.limit.held.fetch_sub(std::mem::take(&mut self.charged), Ordering::Relaxed);
    }

    /// Makes room for the file to reach `end` bytes, within the limit, which counts the
    /// allocation (its capacity, not the file's length): it doubles, but never past what the
    /// limit leaves, and a refusal by the system is an error.
    fn reserve(&mut self, end: usize) -> std::io::Result<()> {
        let old = self.file.capacity();
        if end <= old {
            return Ok(());
        }
        let left = self.limit.max.saturating_sub(self.limit.held.load(Ordering::Relaxed));
        // At least `end`; past what is left only when `end` is, which the limit then refuses.
        let room = old.saturating_mul(2).max(end).min(old.saturating_add(left)).max(end);
        self.limit.take(room - old)?;
        self.charged += room - old;
        // For bytes, `try_reserve_exact` allocates what is asked.
        self.file.try_reserve_exact(room - self.file.len()).map_err(|_| Error::from(ErrorKind::OutOfMemory))?;
        Ok(())
    }
}

impl Drop for Buffer<'_> {
    /// A buffer dropped with its file, as after a refused write or a writer's error, gives back
    /// what it held.
    fn drop(&mut self) {
        self.release();
    }
}

impl Write for Buffer<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let too_far = || Error::from(ErrorKind::InvalidInput);
        let at = usize::try_from(self.at).map_err(|_| too_far())?;
        let end = at.checked_add(bytes.len()).ok_or_else(too_far)?;
        if bytes.is_empty() {
            return Ok(0);
        }
        self.reserve(end)?;
        if at == self.file.len() {
            self.file.extend_from_slice(bytes);
        } else {
            // Over what is there, or past the end, the gap left zero as in a file.
            if end > self.file.len() {
                self.file.resize(end, 0);
            }
            self.file.get_mut(at..end).ok_or_else(too_far)?.copy_from_slice(bytes);
        }
        self.at = end as u64;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Seek for Buffer<'_> {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        let at = match to {
            SeekFrom::Start(at) => Some(at),
            SeekFrom::End(by) => (self.file.len() as u64).checked_add_signed(by),
            SeekFrom::Current(by) => self.at.checked_add_signed(by),
        };
        self.at = at.ok_or_else(|| Error::from(ErrorKind::InvalidInput))?;
        Ok(self.at)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;

    use super::*;
    use crate::raster::{ColorHint, Crs, GeoTransform, Raster, cogwrite, geotiff};
    use crate::{Dataset, Field, FieldType, Geometry, GeometryType, Value};

    /// A request past the limit, however large, is refused without changing what is counted.
    #[test]
    fn a_request_past_the_limit_leaves_the_count_as_it_was() {
        let limit = Limit::new(10);
        limit.take(8).unwrap();
        assert!(limit.take(usize::MAX - 7).is_err());
        assert_eq!(limit.held.load(Ordering::Relaxed), 8);
        assert_eq!(limit.take(2).unwrap(), 10);
        assert!(limit.take(1).is_err());
    }

    /// A buffer holds what a file would after seeks and gaps, never allocates past its limit, and
    /// once a write is refused the limit says so, whatever the writer made of the refusal.
    #[test]
    fn a_buffer_is_a_file_that_stops_at_its_limit() {
        let limit = Limit::new(10);
        let mut b = limit.buffer();
        b.write_all(b"abcd").unwrap();
        b.seek(SeekFrom::Start(1)).unwrap();
        b.write_all(b"XY").unwrap();
        b.seek(SeekFrom::End(2)).unwrap();
        b.write_all(b"z").unwrap();
        assert_eq!(b.file, b"aXYd\0\0z");
        // Another buffer shares what is left of the limit, which counts allocations: the first
        // holds 8 bytes of room for its 7.
        assert_eq!(b.file.capacity(), 8);
        let mut other = limit.buffer();
        other.write_all(b"12").unwrap();
        assert!(!limit.passed());
        assert!(other.write_all(b"3").is_err());
        assert!(limit.passed() && b.file.capacity() <= 10, "{}", b.file.capacity());
        let e = limit.whole(Ok(())).unwrap_err().to_string();
        assert!(e.contains("passes the 10 bytes"), "{e}");
        assert!(b.seek(SeekFrom::Current(-100)).is_err());
        let limit = Limit::new(1000);
        let mut b = limit.buffer();
        for _ in 0..100 {
            b.write_all(b"ten bytes.").unwrap();
            assert!(b.file.capacity() <= 1000, "{}", b.file.capacity());
        }
        assert_eq!(b.into_inner().len(), 1000);
        limit.buffer().write_all(&[0; 1000]).unwrap();
        assert!(!limit.passed(), "a file handed over is no longer counted");
        // Buffers sharing a limit are held to it by what they allocate, not by their lengths,
        // and a file handed on keeps no spare room.
        let limit = Limit::new(1000);
        let mut parts = [limit.buffer(), limit.buffer(), limit.buffer()];
        let mut refused = false;
        for round in 0..200 {
            for i in 0..3 {
                if let Some(part) = parts.get_mut(i) {
                    refused |= part.write_all(&[round as u8; 7]).is_err();
                }
                let allocated: usize = parts.iter().map(|p| p.file.capacity()).sum();
                assert!(allocated <= 1000, "{allocated} bytes allocated");
            }
        }
        assert!(refused && limit.passed());
        let [first, ..] = parts;
        let file = first.into_inner();
        assert_eq!(file.capacity(), file.len());
    }

    /// Counts the items a writer says it has done.
    #[derive(Default)]
    struct Done(AtomicU64);

    impl crate::progress::Probe for Done {
        fn cancelled(&self) -> bool {
            false
        }
        fn begin(&self, _: u64) {}
        fn advance(&self, n: u64) {
            self.0.fetch_add(n, Ordering::Relaxed);
        }
    }

    /// Each vector format a download is made in stops within the first features that pass the
    /// limit, and under it gives the bytes the writer without one gives.
    #[test]
    fn every_download_format_stops_at_the_limit() {
        const ROWS: u64 = 5_000;
        let fields = vec![Field::new("name", FieldType::Text)];
        let mut ds = Dataset::new("Places", GeometryType::Point, fields);
        for i in 0..ROWS {
            let at = crate::Coord::new((i % 360) as f64 - 180.0, (i % 170) as f64 - 85.0);
            ds.push(Some(Geometry::Point(at)), vec![Value::Text(format!("place {i}"))]);
        }
        let formats = ["geojson", "csv", "kml", "kmz", "gpx", "shp", "parquet"];
        let whole = |ext: &str, limit: &Limit| crate::io::write_bytes_noted_within(&ds, ext, Crs::LonLat, "out", limit).map(|w| w.0);
        let text = |format: &str, limit: &Limit| crate::io::text_bytes_within(&ds, format, Some(0), limit);
        let check = |name: &str, early: bool, make: &dyn Fn(&Limit) -> Result<Vec<u8>>| {
            let unlimited = make(&Limit::none()).unwrap();
            assert_eq!(make(&Limit::new(1 << 24)).unwrap(), unlimited, "{name}");
            let done = Arc::new(Done::default());
            let _probe = crate::progress::install(Some(done.clone()));
            let limit = Limit::new(200);
            assert!(make(&limit).is_err() && limit.passed(), "{name}");
            let done = done.0.load(Ordering::Relaxed);
            // A GeoParquet writes a group of rows at a time.
            assert!(!early || done < ROWS / 2, "{name}: {done} of {ROWS} features before it stopped");
        };
        for ext in formats {
            check(ext, ext != "parquet", &|limit| whole(ext, limit));
        }
        for format in ["geojson", "csv", "kml", "gpx", "wkt"] {
            check(format, true, &|limit| text(format, limit));
        }
        // A KMZ and a zipped Shapefile are held to the limit before they are zipped, though their
        // zips would fit.
        for (ext, parts) in [("kmz", &["kml"][..]), ("shp", &["shp", "shx", "dbf"])] {
            let zip = whole(ext, &Limit::none()).unwrap();
            let counted = |name: &str| parts.iter().any(|p| name.ends_with(p));
            let unzipped: usize = crate::zip::read(&zip).unwrap().iter().filter(|e| counted(&e.0)).map(|e| e.1.len()).sum();
            assert!(zip.len() < unzipped - 1, "{ext}");
            assert!(whole(ext, &Limit::new(unzipped - 1)).is_err(), "{ext}");
            // The parts stay counted while the archive is made, so both must fit: with room for
            // half the archive past them it is refused, with room to spare it is the same zip.
            let all: usize = crate::zip::read(&zip).unwrap().iter().map(|e| e.1.len()).sum();
            assert!(whole(ext, &Limit::new(all + zip.len() / 2)).is_err(), "{ext}: parts and archive past the limit");
            assert_eq!(whole(ext, &Limit::new(2 * (all + zip.len()))).unwrap(), zip, "{ext}");
        }
    }

    /// A [`Buffer`] that notes the largest piece offered to it.
    struct Pieces<'a> {
        to: Buffer<'a>,
        largest: usize,
    }

    impl Write for Pieces<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.largest = self.largest.max(bytes.len());
            self.to.write(bytes)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A buffer dropped without being handed over, as a refused write or a writer's error leaves
    /// one, no longer counts against the limit, and a file handed over is released only once.
    #[test]
    fn a_dropped_buffer_gives_back_what_it_held() {
        let limit = Limit::new(10);
        let mut refused = limit.buffer();
        refused.write_all(b"12345678").unwrap();
        assert!(refused.write_all(b"abc").is_err() && limit.passed());
        drop(refused);
        let mut next = limit.buffer();
        next.write_all(&[7; 10]).unwrap();
        assert!(limit.buffer().write_all(b"1").is_err(), "the buffer held is still counted");
        assert_eq!(next.into_inner(), [7; 10]);
        assert_eq!(limit.held.load(Ordering::Relaxed), 0);
    }

    /// One feature whose geometry text alone passes the limit: GeoJSON, KML, GPX and WKT hand it
    /// over a piece at a time, so it is refused at the limit without ever being made whole.
    #[test]
    fn one_large_geometry_stops_at_the_limit_without_its_text_made_whole() {
        const MAX: usize = 10_000;
        let points: Vec<crate::Coord> = (0..20_000).map(|i| crate::Coord::new(f64::from(i % 360) - 180.0, 1.5)).collect();
        for g in [Geometry::MultiPoint(points.clone()), Geometry::Line(vec![points.clone()]), Geometry::Polygon(vec![vec![points]])] {
            let mut ds = Dataset::new("Coast", GeometryType::Polyline, vec![]);
            ds.push(Some(g), vec![]);
            for format in ["geojson", "kml", "gpx", "wkt"] {
                let limit = Limit::new(MAX);
                let mut out = Pieces { to: limit.buffer(), largest: 0 };
                let made = match format {
                    // Without the buffering `write_geojson` adds, to see each piece.
                    "geojson" => crate::io::GeoJsonWriter::new(&mut out, &ds.name, &ds.fields)
                        .and_then(|mut w| w.write(&ds.features).and_then(|()| w.finish().map(drop))),
                    "kml" => crate::interop::write_kml(&ds, None, &mut out),
                    "gpx" => crate::interop::write_gpx(&ds, None, &mut out),
                    _ => crate::interop::write_wkt_csv(&ds, &mut out),
                };
                // No fixed text of these documents is as long as this.
                assert!(out.largest < 200, "{format}: a piece of {} bytes", out.largest);
                assert!(out.to.file.capacity() <= MAX, "{format}: {} bytes held", out.to.file.capacity());
                let e = limit.whole(made).unwrap_err().to_string();
                assert!(limit.passed() && e.contains("passes the 10000 bytes"), "{format}: {e}");
            }
        }
    }

    /// A GeoTIFF and a cloud-optimised one stop at the limit, and under it are unchanged.
    #[test]
    fn a_geotiff_download_stops_at_the_limit() {
        let transform = GeoTransform { origin_x: 0.0, origin_y: 2.0, pixel_width: 0.001, pixel_height: -0.001 };
        let cells: Vec<f32> = (0..1_000_000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 8) as f32).collect();
        let r = Raster::new("r", 1000, 1000, vec![cells], transform, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();
        for cog in [false, true] {
            let make = |limit: &Limit| {
                if cog {
                    cogwrite::write_with_info_within(&r, 0, limit).map(|(bytes, _)| bytes)
                } else {
                    geotiff::write_within(&r, 0, limit)
                }
            };
            let unlimited = make(&Limit::none()).unwrap();
            assert_eq!(make(&Limit::new(unlimited.len())).unwrap(), unlimited, "cog {cog}");
            let done = Arc::new(Done::default());
            let _probe = crate::progress::install(Some(done.clone()));
            let limit = Limit::new(1000);
            assert!(make(&limit).is_err() && limit.passed(), "cog {cog}");
            // At the first piece of the image, of the four or more.
            let done = done.0.load(Ordering::Relaxed);
            assert!(done <= 1, "cog {cog}: {done} pieces");
        }
    }
}
