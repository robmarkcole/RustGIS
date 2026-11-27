//! The rows of a box of one GRIB record, read from its headers and one run of its data section
//! (opengeos/RustGIS#3055): with simple packing and no bitmap, value `k` starts at bit
//! `k × bits` of the data section, so each row of the grid is one run of octets.
//!
//! Read by rows: a single field on a regular latitude/longitude grid, GRIB1 grid point data with
//! simple packing or GRIB2 template 5.0, its rows stored one after another (alternating rows
//! too), without a bitmap. Any other record gives [`Plan::Whole`] with the reason, for the caller
//! to read it whole ([`super::read_bytes`]) and [`cut`] it to the same box, and to say so.
//!
//! Not looked at when rows are read: the record's last four octets ("7777"), which reading them
//! would take another request.

use super::{
    Field, Grid, MAX_BITS, PAST, Packing, SCAN_ALTERNATE, SCAN_J_CONSECUTIVE, Skip, bad, be, date, grib1_grid, grib1_name, grib2_grid,
    grib2_packing, grib2_product, ibm, past_note, signed, to_raster, unpack_from,
};
use crate::Result;
use crate::raster::{GeoTransform, Raster, Uncounted, max_samples};

/// The octets of a record read first, for [`plan`]: its sections before the values.
pub const HEAD: u64 = 4096;

/// What reading a box's rows from a record needs.
pub enum Plan {
    /// One run of the record's octets holds the rows.
    Rows(Box<Rows>),
    /// The record can't be read by rows, and why (a phrase that starts a notice).
    Whole(String),
}

/// The rows of a box in a record: the octets to fetch, and how to read them.
pub struct Rows {
    /// The first and last octets of the record that hold the rows (counted from the record's
    /// first, both included).
    pub run: (u64, u64),
    /// The rows read, and the rows of the record's grid.
    pub rows: (usize, usize),
    /// The rows' own grid: the record's, of fewer rows.
    grid: Grid,
    /// The record's grid.
    full: Grid,
    /// The first of the rows, counted north up in the record's grid.
    row0: usize,
    /// The bits of the run's first octet before the first value.
    skip: u32,
    bits: u32,
    scale: (f64, i64, i64),
    name: String,
    bbox: [f64; 4],
}

/// What the headers of a record that can be read by rows say.
struct Found {
    name: String,
    grid: Grid,
    bits: u32,
    scale: (f64, i64, i64),
    /// Where the packed values start in the record, and the octets its data section gives them.
    data: (u64, u64),
}

const LONG: &str = "headers longer than the 4,096 octets read first can't be read row by row";
const BITMAP: &str = "a record with a bitmap isn't read row by row yet";
const NOT_ONE: &str = "a range that isn't one whole message of one field can't be read row by row";

fn why(s: Skip) -> String {
    match s {
        Skip::Other(w) | Skip::Damaged(w) => format!("{w} can't be read row by row"),
        Skip::TooLarge | Skip::OtherGrid => "this record can't be read row by row".to_string(),
    }
}

/// How to read the rows of `bbox` (`[west, south, east, north]` in degrees) from a record of
/// `len` octets whose first ones (up to [`HEAD`]) are `head`. An error for a box outside the
/// grid's latitudes, or rows of more values than memory allows.
pub fn plan(head: &[u8], len: u64, bbox: [f64; 4]) -> Result<Plan> {
    let found = match head.get(7) {
        Some(1) if u64::from(be(head, 4, 3)) == len => grib1_head(head, len),
        Some(2) if head.get(8..16).is_some_and(|l| l.iter().fold(0u64, |v, &b| (v << 8) | u64::from(b)) == len) => grib2_head(head, len),
        _ => Err(NOT_ONE.to_string()),
    };
    let Found { name, grid, bits, scale, data } = match found {
        Ok(f) => f,
        Err(why) => return Ok(Plan::Whole(why)),
    };
    let whole = |why: &str| Ok(Plan::Whole(why.to_string()));
    if grid.scan & SCAN_J_CONSECUTIVE != 0 {
        return whole("columns stored one after another (j-consecutive scanning) can't be read row by row");
    }
    if bits == 0 {
        return whole("a constant field has no rows to read");
    }
    if bits > MAX_BITS {
        return Ok(Plan::Whole(format!("values of {bits} bits can't be read row by row")));
    }
    let per_row = grid.ni as u64 * u64::from(bits);
    if data.1 < per_row.saturating_mul(grid.nj as u64).div_ceil(8) {
        return whole("a data section that doesn't hold its grid's values can't be read row by row");
    }
    let top = grid.north() + grid.dy / 2.0;
    let Some((r0, r1)) = span(-top, grid.dy, grid.nj, -bbox[3], -bbox[1]) else {
        return Err(bad(format!(
            "the box's latitudes ({}° to {}°) are outside the record's grid ({}° to {}°)",
            bbox[1],
            bbox[3],
            top - grid.nj as f64 * grid.dy,
            top
        )));
    };
    // The rows in the order the record stores them; alternating rows start on an even one, so each
    // keeps the direction it is stored in.
    let (mut j0, j1) = if grid.goes_north() { (grid.nj - 1 - r1, grid.nj - 1 - r0) } else { (r0, r1) };
    if grid.scan & SCAN_ALTERNATE != 0 {
        j0 -= j0 % 2;
    }
    let n = j1 - j0 + 1;
    if n.saturating_mul(grid.ni) > max_samples() {
        return Err(bad(format!("the box's {n} rows of {} values are more than memory allows: give a smaller box", grid.ni)));
    }
    let (from, to) = (j0 as u64 * per_row, (j1 as u64 + 1) * per_row);
    Ok(Plan::Rows(Box::new(Rows {
        run: (data.0 + from / 8, data.0 + to.div_ceil(8) - 1),
        rows: (n, grid.nj),
        grid: Grid { nj: n, ..grid },
        full: grid,
        row0: if grid.goes_north() { grid.nj - 1 - j1 } else { j0 },
        skip: (from % 8) as u32,
        bits,
        scale,
        name,
        bbox,
    })))
}

/// A GRIB1 record's headers, when it can be read by rows; else why not.
fn grib1_head(b: &[u8], len: u64) -> std::result::Result<Found, String> {
    let section = |at: usize| b.get(at..at + (be(b, at, 3) as usize).max(3)).ok_or(LONG);
    let pds = section(8)?;
    let flags = pds.get(7).copied().unwrap_or(0);
    if pds.len() < 28 || flags & 0x80 == 0 {
        return Err("a record without its own grid can't be read row by row".into());
    }
    if flags & 0x40 != 0 {
        return Err(BITMAP.into());
    }
    let gds = section(8 + pds.len())?;
    let grid = grib1_grid(gds).map_err(why)?;
    let at = 8 + pds.len() + gds.len();
    let bds = b.get(at..at + 11).ok_or(LONG)?;
    if bds[3] & 0xc0 != 0 {
        return Err("spherical harmonics and second-order packing can't be read row by row".into());
    }
    let size = u64::from(be(bds, 0, 3));
    if at as u64 + size + 4 != len {
        return Err(NOT_ONE.into());
    }
    Ok(Found {
        name: grib1_name(pds),
        grid,
        bits: u32::from(bds[10]),
        scale: (ibm(be(bds, 6, 4)), signed(bds, 4, 2), signed(pds, 26, 2)),
        data: (at as u64 + 11, size.saturating_sub(11)),
    })
}

/// A GRIB2 record's headers, when it can be read by rows; else why not. Each of sections 1 and
/// 3 to 6 once, as the whole reader asks of a field.
fn grib2_head(b: &[u8], len: u64) -> std::result::Result<Found, String> {
    const DAMAGED: &str = "a record with damaged sections can't be read row by row";
    let (mut reference, mut grid, mut product, mut packing, mut bitmap) = (None, None::<Grid>, None, None, None);
    let (mut at, mut template) = (16usize, 0);
    loop {
        let start = b.get(at..at + 5).ok_or(LONG)?;
        let size = be(start, 0, 4) as usize;
        if start[4] == 7 {
            let (Some(reference), Some(grid), Some(product), Some(packing), Some(255)) = (reference, grid, product, packing, bitmap) else {
                return Err(if bitmap.is_some_and(|b| b != 255) { BITMAP } else { DAMAGED }.into());
            };
            let (count, r, e, d, Packing::Simple(bits)) = packing else {
                return Err(format!("complex packing (template 5.{template}) can't be read row by row"));
            };
            if at as u64 + size as u64 + 4 != len || size < 5 {
                return Err(NOT_ONE.into());
            }
            if grid.ni.checked_mul(grid.nj) != Some(count) {
                return Err(DAMAGED.into());
            }
            let name = format!("{reference}:discipline={}:{product}", b.get(6).copied().unwrap_or(255));
            return Ok(Found { name, grid, bits, scale: (r, e, d), data: (at as u64 + 5, size as u64 - 5) });
        }
        let s = b.get(at..at.saturating_add(size)).filter(|_| size >= 5).ok_or(if size < 5 { DAMAGED } else { LONG })?;
        let again = match start[4] {
            1 => reference.replace(date(i64::from(be(s, 12, 2)), be(s, 14, 1), be(s, 15, 1), be(s, 16, 1))).is_some(),
            3 => grid.replace(grib2_grid(s).map_err(why)?).is_some(),
            4 => product.replace(grib2_product(s)).is_some(),
            5 => {
                template = be(s, 9, 2);
                packing.replace(grib2_packing(s).map_err(why)?).is_some()
            }
            6 => bitmap.replace(s.get(5).copied().unwrap_or(0)).is_some(),
            _ => false,
        };
        if again {
            return Err(NOT_ONE.into());
        }
        at += size;
    }
}

impl Rows {
    /// The rows' raster, cut to the box's columns, from the octets of [`Rows::run`]: laid out as
    /// the whole record is (north up, a whole turn from −180°), with its band name and notices.
    pub fn read(&self, run: &[u8], name: &str) -> Result<Raster> {
        let points = self.grid.ni * self.grid.nj;
        let at = (points, &self.grid);
        let (values, rounded, past) =
            unpack_from(run, self.skip, (points, self.bits), self.scale, None, at, Uncounted::new(None, None)).map_err(|s| bad(why(s)))?;
        let mut r = to_raster(vec![Field { name: self.name.clone(), grid: self.grid, values, rounded, past }], name)?;
        r.lost.extend(self.full.notes());
        cut_rows(r, self.full.north() + self.full.dy / 2.0, (self.row0, self.full.nj), self.bbox, true)
    }
}

/// A record read whole, cut to the cells a box touches (`[west, south, east, north]` in degrees):
/// what [`Rows::read`] gives of it. An error for a box outside it.
pub fn cut(r: Raster, bbox: [f64; 4]) -> Result<Raster> {
    let (top, rows) = (r.transform.origin_y, r.height as usize);
    cut_rows(r, top, (0, rows), bbox, false)
}

/// [`cut`] of `r`, rows `row0..` of a grid of `of` rows whose north edge is at `top`. `unmasked`:
/// whether `r` is of simple packing without a bitmap ([`Rows::read`]'s), where a cell without a
/// value is one past `f32`'s range, so those are counted over the cells kept; else a count made
/// over the whole record is said as the record's, without the number.
fn cut_rows(r: Raster, top: f64, (row0, of): (usize, usize), bbox: [f64; 4], unmasked: bool) -> Result<Raster> {
    let (t, w, h) = (r.transform, r.width as usize, r.height as usize);
    let dy = -t.pixel_height;
    let columns = span(t.origin_x, t.pixel_width, w, bbox[0], bbox[2]);
    let rows = span(-top, dy, of, -bbox[3], -bbox[1]).filter(|(a, b)| *a >= row0 && *b < row0 + h);
    let (Some((c0, c1)), Some((r0, r1))) = (columns, rows) else {
        return Err(bad(format!(
            "the box ({}, {}, {}, {}) is outside the record's grid ({}, {}, {}, {})",
            bbox[0],
            bbox[1],
            bbox[2],
            bbox[3],
            t.origin_x,
            top - of as f64 * dy,
            t.origin_x + w as f64 * t.pixel_width,
            top
        )));
    };
    let mut bands = Vec::with_capacity(r.bands.len());
    for b in &r.bands {
        let mut band = crate::raster::grid::<f32>(c1 - c0 + 1, r1 - r0 + 1)?;
        for row in r0 - row0..=r1 - row0 {
            band.extend_from_slice(b.data.get(row * w + c0..=row * w + c1).unwrap_or_default());
        }
        bands.push(band);
    }
    let size = |n: usize| u32::try_from(n).map_err(|_| bad("the grid is too large"));
    let transform = GeoTransform { origin_x: t.origin_x + c0 as f64 * t.pixel_width, origin_y: top - r0 as f64 * dy, ..t };
    let mut out = Raster::new(
        r.name.clone(),
        size(c1 - c0 + 1)?,
        size(r1 - r0 + 1)?,
        bands,
        transform,
        r.crs,
        r.nodata,
        Default::default(),
        "Float32",
    )?;
    for (b, from) in out.bands.iter_mut().zip(&r.bands) {
        b.name.clone_from(&from.name);
    }
    // Counted over more cells than are kept: a mark, not a count.
    if r.rounded.iter().any(|n| *n > 0) {
        out.rounded_some = r.rounded.iter().map(|n| *n > 0).collect();
    }
    out.lost = r.lost;
    if out.lost.iter().any(|n| n.as_str().contains(PAST)) {
        out.lost.retain(|n| !n.as_str().contains(PAST));
        let past = out.bands.iter().flat_map(|b| &b.data).filter(|v| v.is_nan()).count();
        if !unmasked {
            let said = format!("values {PAST} in the whole record have no value: any among the box's cells are empty");
            out.lost.push(crate::loss::Note::loss(crate::loss::LossCode::DroppedValue, said));
        } else if past > 0 {
            out.lost.push(past_note(past as u64));
        }
    }
    // A box that reaches more than a cell past the grid (a global grid's edges fall within one of
    // the world's): the layer is smaller than the box, and says so.
    let (east, south) = (t.origin_x + w as f64 * t.pixel_width, top - of as f64 * dy);
    if bbox[0] < t.origin_x - t.pixel_width || bbox[2] > east + t.pixel_width || bbox[1] < south - dy || bbox[3] > top + dy {
        out.lost.push("the box reaches past the record's grid: the layer ends where the grid does".to_string());
    }
    Ok(out)
}

/// The cells `a..=b`, of `n` cells of `step` from `origin`, that the interval from `lo` to `hi`
/// reaches into; `None` when it reaches into none.
fn span(origin: f64, step: f64, n: usize, lo: f64, hi: f64) -> Option<(usize, usize)> {
    let a = ((lo - origin) / step).floor().max(0.0);
    let b = (((hi - origin) / step).ceil() - 1.0).min(n as f64 - 1.0);
    (a <= b).then_some((a as usize, b as usize))
}
