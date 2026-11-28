//! A block diagram cut to polygons (#2318): the outline of the polygons a layer's `where` picks,
//! in Web Mercator, that the ground is cut to and the walls stand along.

use std::sync::{Arc, Mutex, Weak};

use rustgis_data::{Feature, Geometry};
use rustgis_geom::{Coord, lonlat_to_merc};
use rustgis_map::Map;

/// The most sides an outline keeps: a wall's side is numbered in 16 bits (`block::Edge`).
const MAX_SIDES: usize = u16::MAX as usize;

/// How near two corners in a row may lie, as a share of the outline's longer side: nearer, the
/// second is left out. A third of the finest settled mesh's cell (640 a side), so the outline
/// keeps every bend the ground's mesh can show.
const SPACING: f64 = 1.0 / 2000.0;

/// The most entries the sides' row index holds (each side once in every row it spans): 16 MB.
const ROW_BUDGET: usize = 4_000_000;

/// One side of an outline: its ends (its polygon's ground on their left: anticlockwise round an
/// outer ring, clockwise round a hole), its ring, and how far along the ring it begins (Web
/// Mercator metres).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Side {
    pub a: Coord,
    pub z: Coord,
    pub ring: u32,
    pub along: f64,
}

/// The rings of a block's polygons (Web Mercator; outer rings anticlockwise, holes clockwise,
/// each polygon's rings in a row), their sides in order, and the sides by row of the outline's
/// box, for [`Outline::contains`] and [`Outline::near`]. A hole is a well: its ground isn't
/// drawn, its walls face into it, and its floor is drawn at the base.
#[derive(Debug, PartialEq)]
pub struct Outline {
    sides: Vec<Side>,
    /// Each ring's first side, its count of sides and its length round.
    rings: Vec<(usize, usize, f64)>,
    /// Each ring's polygon (a number its outer ring and its holes share) and whether it is a hole.
    kinds: Vec<(u32, bool)>,
    extent: [f64; 4],
    rows: Vec<Vec<u32>>,
    /// How near two corners in a row were let lie (Web Mercator metres; [`SPACING`] of the longer
    /// side, or more when the sides would have passed [`MAX_SIDES`]), and the rings left out
    /// (the smallest) when even that wasn't enough.
    spacing: f64,
    coarsened: bool,
    dropped: usize,
    /// How many holes the outline keeps, each cut as a well.
    holes: usize,
    /// How many holes lay outside their own polygon's outer ring: left out, as they cut nothing.
    stray: usize,
    /// How many rings with an area had none left once thinned (narrower than the spacing).
    narrow: usize,
}

/// The rows, layer, queries and outline read last ([`Outline::of`]), kept while the layer's rows
/// stay the same: the outline is read again only after an edit or a new query. One that can't be
/// read is kept too, so a block whose `where` picks nothing isn't looked for again every frame.
type Kept = (Weak<Vec<Feature>>, String, Option<String>, Option<String>, Option<Arc<Outline>>);
static LAST: Mutex<Vec<Kept>> = Mutex::new(Vec::new());
/// How many outlines [`LAST`] keeps (maps drawn side by side, a layout's frames).
const KEPT: usize = 4;

impl Outline {
    /// The outline of the polygons of `layer` on `map` (as [`super::walls_layer`] takes a layer)
    /// that `filter` picks among the rows its definition query keeps (all of them without one).
    /// `None` when there is no such layer, the filter doesn't read, or it picks no polygon.
    pub fn of(map: &Map, layer: &str, filter: Option<&str>) -> Option<Arc<Outline>> {
        let l = super::walls_layer(map, layer)?;
        // Its places that can't be given in longitude/latitude are counted and said, as a walls
        // layer's are.
        crate::note_held_read(l);
        let rows = l.dataset.features.allocation_key();
        let same = |k: &Kept| k.0.ptr_eq(&rows) && k.1 == l.name && k.2 == l.definition_query && k.3.as_deref() == filter;
        if let Some(o) = LAST.lock().ok().and_then(|last| last.iter().find(|k| same(k)).map(|k| k.4.clone())) {
            return o;
        }
        let keep = l.query_rows(&l.dataset);
        let pick = rustgis_data::rows::Query::compile(filter, &l.dataset).ok()?;
        let env = rustgis_data::query::Env::default();
        let mut polygons: Vec<Vec<Vec<Coord>>> = Vec::new();
        // In longitude/latitude whatever system the layer holds its coordinates in, as the walls
        // read it (`block::units`); a polygon that can't be given there isn't drawn by the map either.
        let to = l.dataset.shown_conversion().ok();
        for (row, ft) in l.dataset.features.rows().enumerate() {
            if keep.as_ref().is_some_and(|k| !k.get(row).copied().unwrap_or(false)) || !pick.matches(&l.dataset, row, &env) {
                continue;
            }
            let Some(lonlat) = ft.geometry().zip(to.as_ref()).and_then(|(g, to)| rustgis_data::Dataset::converted_to_lonlat(to, g)) else {
                continue;
            };
            let Geometry::Polygon(parts) = &*lonlat else { continue };
            polygons.extend(parts.iter().map(|p| p.iter().map(|r| r.iter().map(|c| lonlat_to_merc(*c)).collect()).collect()));
        }
        let o = Outline::from_polygons(polygons).map(Arc::new);
        if let Ok(mut last) = LAST.lock() {
            last.retain(|k| k.0.strong_count() > 0 && !same(k));
            if last.len() >= KEPT {
                last.remove(0);
            }
            last.push((rows, l.name.clone(), l.definition_query.clone(), filter.map(str::to_string), o.clone()));
        }
        o
    }

    /// The outline of `rings`, each a polygon without holes ([`Outline::from_polygons`]).
    #[cfg(test)]
    pub(crate) fn from_rings(rings: Vec<Vec<Coord>>) -> Option<Outline> {
        Outline::from_polygons(rings.into_iter().map(|r| vec![r]).collect())
    }

    /// The outline of `polygons` (each its outer ring, then its holes; Web Mercator, either way
    /// round, closed or not), corners nearer than [`SPACING`] of its longer side to the last one
    /// kept left out (more spaced still if it would have over [`MAX_SIDES`] sides). `None` when
    /// no outer ring has an area.
    pub(crate) fn from_polygons(polygons: Vec<Vec<Vec<Coord>>>) -> Option<Outline> {
        // Only rings with an area count: one without gives no ground and sets no spacing. Each
        // keeps its polygon's number and whether it is a hole; a polygon whose outer ring has no
        // area gives no holes either. A hole outside its outer ring (an invalid polygon) would
        // close on its own as ground ([`crate::block`]'s cut follows a polygon's rings): left out
        // and counted.
        let (mut rings, mut kinds): (Vec<Vec<Coord>>, Vec<(u32, bool)>) = (Vec::new(), Vec::new());
        let mut stray = 0;
        for (p, polygon) in polygons.into_iter().enumerate() {
            let mut outer = 0;
            for (i, r) in polygon.into_iter().enumerate() {
                let r: Vec<Coord> = r.into_iter().filter(|c| c.x.is_finite() && c.y.is_finite()).collect();
                if r.len() >= 3 && area2(&r).abs() > 0.0 && area2(&r).is_finite() {
                    if i == 0 {
                        outer = rings.len();
                    } else if !r.first().is_some_and(|c| within(&rings[outer], *c)) {
                        stray += 1;
                        continue;
                    }
                    rings.push(r);
                    kinds.push((p as u32, i > 0));
                } else if i == 0 {
                    break;
                }
            }
        }
        let (mut x0, mut y0, mut x1, mut y1) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
        for c in rings.iter().flatten() {
            (x0, y0, x1, y1) = (x0.min(c.x), y0.min(c.y), x1.max(c.x), y1.max(c.y));
        }
        if !(x0 < x1 && y0 < y1) {
            return None;
        }
        let longer = (x1 - x0).max(y1 - y0);
        let first = longer * SPACING;
        let mut gap = first;
        // Each ring thinned to an eighth of its own size at most, so a small polygon far from the
        // others keeps a shape.
        let size = |r: &[Coord]| {
            let (xs, ys) = (r.iter().map(|c| c.x), r.iter().map(|c| c.y));
            (xs.clone().fold(f64::NEG_INFINITY, f64::max) - xs.fold(f64::INFINITY, f64::min))
                .max(ys.clone().fold(f64::NEG_INFINITY, f64::max) - ys.fold(f64::INFINITY, f64::min))
        };
        let count = |t: &[(usize, Vec<Coord>)]| t.iter().map(|r| r.1.len()).sum::<usize>();
        let mut thinned = loop {
            let thinned: Vec<(usize, Vec<Coord>)> =
                rings.iter().enumerate().filter_map(|(i, r)| Some((i, thin(r, gap.min(size(r) / 8.0))?))).collect();
            if count(&thinned) <= MAX_SIDES || gap > longer {
                break thinned;
            }
            gap *= 2.0;
        };
        // A hole whose outer ring was left out goes with it: alone it would cut ground no ring
        // holds. Counted with the ring: narrow here, past the sides below.
        let orphans = |t: &mut Vec<(usize, Vec<Coord>)>| {
            let outers: std::collections::HashSet<u32> = t.iter().filter(|(i, _)| !kinds[*i].1).map(|(i, _)| kinds[*i].0).collect();
            t.retain(|(i, _)| !kinds[*i].1 || outers.contains(&kinds[*i].0));
        };
        orphans(&mut thinned);
        let narrow = rings.len() - thinned.len();
        // Still too many (thousands of small rings): the largest that fit are kept, put back in
        // order so each polygon's rings stay in a row.
        let all = thinned.len();
        if count(&thinned) > MAX_SIDES {
            thinned.sort_by(|a, b| area2(&b.1).total_cmp(&area2(&a.1)));
            let mut used = 0;
            thinned.retain(|r| {
                let fits = used + r.1.len() <= MAX_SIDES;
                used += if fits { r.1.len() } else { 0 };
                fits
            });
            thinned.sort_by_key(|r| r.0);
            orphans(&mut thinned);
        }
        let dropped = all - thinned.len();
        // The widest gap a ring was thinned at: past an eighth of the largest ring, `gap` only
        // doubled on (the rings' own eighths kept their corners), so it isn't said as the spacing.
        let spacing = gap.min(rings.iter().map(|r| size(r)).fold(0.0, f64::max) / 8.0);
        // The box is the kept rings' (a ring without an area, or left out, doesn't widen it).
        (x0, y0, x1, y1) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
        for c in thinned.iter().flat_map(|(i, _)| &rings[*i]) {
            (x0, y0, x1, y1) = (x0.min(c.x), y0.min(c.y), x1.max(c.x), y1.max(c.y));
        }
        let (mut sides, mut starts) = (Vec::new(), Vec::new());
        let kept: Vec<(u32, bool)> = thinned.iter().map(|(i, _)| kinds[*i]).collect();
        for (i, (from, r)) in thinned.iter_mut().enumerate() {
            // [`thin`] turns every ring anticlockwise: a hole is turned back, its ground on the left.
            if kinds[*from].1 {
                r.reverse();
            }
            let first = sides.len();
            let mut along = 0.0;
            for (j, &a) in r.iter().enumerate() {
                let z = r[(j + 1) % r.len()];
                sides.push(Side { a, z, ring: i as u32, along });
                along += (z.x - a.x).hypot(z.y - a.y);
            }
            starts.push((first, r.len(), along));
        }
        if sides.is_empty() {
            return None;
        }
        // Rows enough for a few sides each, fewer when tall sides would fill [`ROW_BUDGET`].
        let tall: f64 = sides.iter().map(|s| (s.a.y - s.z.y).abs() / (y1 - y0)).sum();
        let n = (sides.len() / 4).clamp(1, 4096).min((ROW_BUDGET.saturating_sub(sides.len()) as f64 / tall.max(1e-9)) as usize).max(1);
        let mut rows = vec![Vec::new(); n];
        let row = |y: f64| (((y - y0) / (y1 - y0) * n as f64).floor().max(0.0) as usize).min(n - 1);
        for (k, s) in sides.iter().enumerate() {
            let (lo, hi) = (row(s.a.y.min(s.z.y)), row(s.a.y.max(s.z.y)));
            rows.iter_mut().take(hi + 1).skip(lo).for_each(|r| r.push(k as u32));
        }
        Some(Outline {
            sides,
            rings: starts,
            holes: kept.iter().filter(|k| k.1).count(),
            stray,
            kinds: kept,
            extent: [x0, y0, x1, y1],
            rows,
            spacing,
            coarsened: spacing > first,
            dropped,
            narrow,
        })
    }

    /// How many sides the outline keeps.
    pub fn side_count(&self) -> usize {
        self.sides.len()
    }

    /// How near two corners in a row were let lie (Web Mercator metres), and whether that is more
    /// than the usual third of the finest mesh cell because the polygons had too many corners.
    pub fn spacing(&self) -> (f64, bool) {
        (self.spacing, self.coarsened)
    }

    /// How many of the polygons' rings were left out (the smallest) to keep the outline under
    /// 65 535 sides.
    pub fn dropped(&self) -> usize {
        self.dropped
    }

    /// How many holes the outline keeps, each cut as a well with its own walls.
    pub fn holes(&self) -> usize {
        self.holes
    }

    /// How many holes lay outside their own polygon's outer ring (an invalid polygon): left out.
    pub fn stray(&self) -> usize {
        self.stray
    }

    /// Ring `r`'s polygon and whether it is a hole.
    pub(crate) fn kind(&self, r: u32) -> (u32, bool) {
        self.kinds.get(r as usize).copied().unwrap_or((r, false))
    }

    /// Each hole's sides (their numbers), in a row round it.
    pub(crate) fn hole_sides(&self) -> impl Iterator<Item = std::ops::Range<usize>> + '_ {
        self.rings.iter().zip(&self.kinds).filter(|(_, k)| k.1).map(|(r, _)| r.0..r.0 + r.1)
    }

    /// How many of the polygons' rings were left out as narrower than the spacing: thinned, no
    /// area was left.
    pub fn narrow(&self) -> usize {
        self.narrow
    }

    /// The box round the outline, `[xmin, ymin, xmax, ymax]` (Web Mercator).
    pub fn extent(&self) -> [f64; 4] {
        self.extent
    }

    pub(crate) fn sides(&self) -> &[Side] {
        &self.sides
    }

    /// The sides before and after side `k` round its ring.
    pub(crate) fn neighbours(&self, k: usize) -> (usize, usize) {
        let Some(s) = self.sides.get(k) else { return (k, k) };
        let (first, len, _) = self.rings.get(s.ring as usize).copied().unwrap_or((k, 1, 0.0));
        let (i, len) = (k - first, len.max(1));
        (first + (i + len - 1) % len, first + (i + 1) % len)
    }

    /// The length round side `k`'s ring (Web Mercator metres).
    pub(crate) fn around(&self, k: usize) -> f64 {
        self.sides.get(k).and_then(|s| self.rings.get(s.ring as usize)).map_or(0.0, |r| r.2)
    }

    fn rows_of(&self, y0: f64, y1: f64) -> std::ops::RangeInclusive<usize> {
        let [_, by0, _, by1] = self.extent;
        let n = self.rows.len();
        let row = |y: f64| ((((y - by0) / (by1 - by0)) * n as f64).floor().max(0.0) as usize).min(n - 1);
        row(y0)..=row(y1)
    }

    /// Whether `m` lies inside the outline: inside one of its polygons and none of its holes.
    pub fn contains(&self, m: Coord) -> bool {
        self.inside_ring(m, |_| true)
    }

    /// Whether `m` lies inside one of the polygons, of their rings `counted` keeps.
    pub(crate) fn inside_ring(&self, m: Coord, counted: impl Fn(u32) -> bool) -> bool {
        let [x0, y0, x1, y1] = self.extent;
        if !(m.x >= x0 && m.x <= x1 && m.y >= y0 && m.y <= y1) {
            return false;
        }
        // An outer ring runs anticlockwise and a hole clockwise: a polygon round `m` winds it once,
        // and once back when `m` is in one of its holes. A row lists its sides in order, and a
        // polygon's sides follow one another, so each polygon's winding is summed in turn.
        let (mut polygon_of, mut winding) = (u32::MAX, 0);
        for &k in self.rows.get(*self.rows_of(m.y, m.y).start()).into_iter().flatten() {
            let Some(Side { a, z, ring, .. }) = self.sides.get(k as usize).copied() else { continue };
            let polygon = self.kind(ring).0;
            if polygon != polygon_of {
                if winding > 0 {
                    return true;
                }
                (polygon_of, winding) = (polygon, 0);
            }
            if counted(ring) && (a.y > m.y) != (z.y > m.y) && m.x < a.x + (m.y - a.y) / (z.y - a.y) * (z.x - a.x) {
                winding += if z.y > a.y { 1 } else { -1 };
            }
        }
        winding > 0
    }

    /// The sides whose box meets the box from `lo` to `hi`, each once, into `out` (emptied first).
    pub(crate) fn near(&self, lo: Coord, hi: Coord, out: &mut Vec<u32>) {
        out.clear();
        let [x0, y0, x1, y1] = self.extent;
        if hi.x < x0 || lo.x > x1 || hi.y < y0 || lo.y > y1 {
            return;
        }
        for r in self.rows_of(lo.y, hi.y) {
            for &k in self.rows.get(r).into_iter().flatten() {
                let Some(s) = self.sides.get(k as usize) else { continue };
                if s.a.x.max(s.z.x) >= lo.x && s.a.x.min(s.z.x) <= hi.x && s.a.y.max(s.z.y) >= lo.y && s.a.y.min(s.z.y) <= hi.y {
                    out.push(k);
                }
            }
        }
        out.sort_unstable();
        out.dedup();
    }
}

/// `ring` without a closing corner, corners nearer than `gap` to the last kept left out, turned
/// anticlockwise. `None` when under 3 corners or no area is left.
fn thin(ring: &[Coord], gap: f64) -> Option<Vec<Coord>> {
    let mut out: Vec<Coord> = Vec::with_capacity(ring.len());
    for &c in ring {
        if out.last().is_none_or(|p| (c.x - p.x).hypot(c.y - p.y) >= gap) {
            out.push(c);
        }
    }
    while out.len() > 1 && out.last().zip(out.first()).is_some_and(|(l, f)| (l.x - f.x).hypot(l.y - f.y) < gap) {
        out.pop();
    }
    let area2 = area2(&out);
    if out.len() < 3 || area2.abs() <= 0.0 || !area2.is_finite() {
        return None;
    }
    if area2 < 0.0 {
        out.reverse();
    }
    Some(out)
}

/// Whether `p` lies inside `ring`, even-odd.
fn within(ring: &[Coord], p: Coord) -> bool {
    let mut inside = false;
    for (a, z) in ring.iter().zip(ring.iter().cycle().skip(1)) {
        if (a.y > p.y) != (z.y > p.y) && p.x < a.x + (p.y - a.y) / (z.y - a.y) * (z.x - a.x) {
            inside = !inside;
        }
    }
    inside
}

/// Twice the signed area of `ring` (positive anticlockwise).
fn area2(ring: &[Coord]) -> f64 {
    ring.iter().zip(ring.iter().cycle().skip(1)).map(|(a, b)| a.x * b.y - b.x * a.y).sum()
}
