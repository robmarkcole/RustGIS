//! A layer's GPU mesh cut into square tiles (#2541), each tessellated on its own and drawn only
//! inside its square (the GPU marks the square in the stencil, as it draws vector tiles), so a
//! layer's tiles can be tessellated on several threads.
//!
//! The tiles are a grid of squares, a power of two map units on a side: the one nearest
//! [`TILE_PX`] physical pixels at the scene's scale. Each tile's mesh has the tile's south-west
//! corner as its origin. A feature reaches the tiles its extent touches, grown by half its
//! stroke's width as tessellated ([`crate::mesh`]'s `tessellation_width`): a stroke near a
//! border spills into the next tile, and that tile draws the spill. A feature lying wholly
//! within a tile grown by that much is tessellated as it is. A larger one is cut to the grown
//! tile first, so a big polygon isn't tessellated whole in every tile it touches:
//! - a fill's rings are clipped to it (Sutherland–Hodgman, ring by ring, which keeps the
//!   even-odd rule inside the tile);
//! - a line, or a polygon's outline, is cut into the parts inside it. A part ends on the grown
//!   tile's edge, so its round cap lies outside the square, under the stencil: no seams.
//!
//! Point markers aren't tiled: a marker near a border would be cut by the square. A layer's
//! markers are one command after its tiles, as before.
//!
//! The scene is the same on any number of threads: the tiles come out in grid order. A small
//! layer is meshed on one thread, and the threads are no more than the memory set aside for
//! drawing holds ([`threads_for`]).
//!
//! A tile's mesh hangs only on the tile and the features reaching it, not on where the scene's
//! area lies: a feature is taken when its stroke reaches one of the area's tiles, not when it
//! reaches the area itself. So a pan keeps the meshes of the tiles still in the scene
//! ([`Tiling::kept`], from [`crate::layer_meshes`]) and meshes only the tiles it brings in. A
//! stroke reaching a tile side or more past its feature (wider than about 1,000 px) could reach
//! in from a feature left out, which the scene doesn't look at: a layer that has one keeps no
//! tiles.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use rustgis_data::Geometry;
use rustgis_geom::{Coord, Extent};
use rustgis_map::Symbol;

use crate::gpu_scene::{DrawCmd, MeshVertex};
use crate::mesh::TessError;

/// About how many physical pixels a tile is on a side.
pub(crate) const TILE_PX: f64 = 512.0;

/// The most tiles a layer is cut into; past it (a scene far larger than a screen) the layer is
/// one mesh, as before.
const MAX_TILES: usize = 4096;

/// The most (tile, feature) pairs a layer's tiles list: past it (many features each over many
/// tiles), the layer is made one mesh, so the lists stay within 8 MB of indices.
const MAX_MEMBERS: usize = 1 << 20;

/// A layer estimated at fewer vertices than this is meshed on one thread: starting threads would
/// cost more than they save.
const THREADS_FROM_VERTICES: usize = 50_000;

/// The bytes a mesh vertex takes while a tile is tessellated: the vertex (40 bytes), its share of
/// the indices and lyon's scratch.
const SCRATCH_PER_VERTEX: usize = 96;

/// The threads to mesh `tiles` on: one for little work, else as many as the tiles and the user's
/// thread count allow, and no more than [`rustgis_data::memory::thread_scratch`] holds beyond one
/// thread's, each meshing a tile as big as the biggest (by the features' estimates, each counted
/// whole in every tile it reaches).
fn threads_for(features: &[Feature<'_>], tiles: &[&(Tile, Vec<usize>)]) -> usize {
    let estimate =
        |members: &[usize]| members.iter().filter_map(|k| features.get(*k)).map(|f| f.estimate).fold(0usize, usize::saturating_add);
    let total = tiles.iter().map(|(_, m)| estimate(m)).fold(0usize, usize::saturating_add);
    if total < THREADS_FROM_VERTICES {
        return 1;
    }
    let biggest = tiles.iter().map(|(_, m)| estimate(m)).max().unwrap_or(0).saturating_mul(SCRATCH_PER_VERTEX).max(1);
    let affordable = (rustgis_data::memory::thread_scratch() / biggest).saturating_add(1);
    rustgis_data::threads::for_items(tiles.len()).min(affordable).max(1)
}

/// The side of the tiles, map units: the power of two nearest [`TILE_PX`] physical pixels at
/// `map_per_px` map units per physical pixel. `None` for a scale that isn't a positive number.
pub(crate) fn tile_side(map_per_px: f64) -> Option<f64> {
    let side = 2f64.powf((TILE_PX * map_per_px).log2().round());
    (map_per_px.is_finite() && map_per_px > 0.0 && side.is_normal()).then_some(side)
}

/// A tile of the grid, by column and row (x and y over the side, rounded down).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Tile {
    row: i64,
    column: i64,
}

impl Tile {
    /// Its south-west corner, map units: its mesh's origin.
    fn corner(self, side: f64) -> [f64; 2] {
        [self.column as f64 * side, self.row as f64 * side]
    }
}

/// A tile meshed for a scene, kept for the next one at the same scale ([`crate::layer_meshes`]).
pub(crate) struct MadeTile {
    tile: Tile,
    /// The least vertex limit it meshes within: its vertices, or more where a feature taken
    /// whole was estimated at more. Under a lower limit, meshing it again would give up.
    need: usize,
    /// Its Mesh command, sharing its data with the scenes drawing it; `None` when nothing draws
    /// in it.
    pub(crate) cmd: Option<DrawCmd>,
}

impl MadeTile {
    /// The tile sharing this one's mesh.
    pub(crate) fn share(&self) -> MadeTile {
        MadeTile { tile: self.tile, need: self.need, cmd: self.cmd.as_ref().and_then(DrawCmd::share) }
    }

    /// Its mesh's vertices.
    fn vertices(&self) -> usize {
        match &self.cmd {
            Some(DrawCmd::Mesh { vertices, .. }) => vertices.len(),
            _ => 0,
        }
    }
}

/// How a layer's lines and polygons are tiled for a scene.
pub(crate) struct Tiling<'a> {
    /// The scene's cull: its tiles are those it touches.
    pub area: &'a Extent,
    /// The tiles made for an earlier scene of the layer as it is, at this scale, in grid order:
    /// those still in this scene are taken as they are.
    pub kept: &'a [MadeTile],
    /// Filled with this scene's tiles, in grid order, to keep; left empty when they can't be
    /// taken again.
    pub made: &'a mut Vec<MadeTile>,
}

/// The tiles touching `area`: their columns and rows. `None` for an area that isn't finite.
fn grid(area: &Extent, side: f64) -> Option<(std::ops::RangeInclusive<i64>, std::ops::RangeInclusive<i64>)> {
    let finite = [area.min_x, area.min_y, area.max_x, area.max_y].iter().all(|v| v.is_finite());
    let index = |v: f64| (v / side).floor() as i64;
    (finite && area.min_x <= area.max_x && area.min_y <= area.max_y)
        .then(|| (index(area.min_x)..=index(area.max_x), index(area.min_y)..=index(area.max_y)))
}

/// The extent the features of a scene's tiles lie within or near: the tiles touching `area`,
/// grown by a tile, so a feature whose stroke reaches less than a tile past it reaches them only
/// from in there. `None` at a scale with no tiles.
pub(crate) fn feature_area(area: &Extent, map_per_px: f64) -> Option<Extent> {
    let side = tile_side(map_per_px)?;
    let (columns, rows) = grid(area, side)?;
    let edge = |i: i64| i as f64 * side;
    Some(Extent::new(
        edge(columns.start().saturating_sub(1)),
        edge(rows.start().saturating_sub(1)),
        edge(columns.end().saturating_add(2)),
        edge(rows.end().saturating_add(2)),
    ))
}

/// One feature of a layer to tile: its geometry, symbol, its estimate of vertices, its extent
/// and how far its stroke reaches past it (map units).
pub(crate) struct Feature<'a> {
    pub geometry: &'a Geometry,
    pub symbol: Symbol,
    pub estimate: usize,
    pub extent: Extent,
    pub reach: f64,
}

/// The tiles touching `area` that a layer's features reach, each with the features (by index)
/// reaching it, in grid order. A tile lists every feature reaching it, wherever in the tile, so
/// its features don't hang on where `area` lies. `None` when they would be more than
/// [`MAX_TILES`], or list more than [`MAX_MEMBERS`] features in all.
fn buckets(features: &[Feature<'_>], area: &Extent, side: f64) -> Option<Vec<(Tile, Vec<usize>)>> {
    let index = |v: f64| (v / side).floor() as i64;
    let (in_columns, in_rows) = grid(area, side)?;
    let mut tiles: BTreeMap<Tile, Vec<usize>> = BTreeMap::new();
    let mut members = 0usize;
    for (k, f) in features.iter().enumerate() {
        let r = f.reach;
        let grown = Extent::new(f.extent.min_x - r, f.extent.min_y - r, f.extent.max_x + r, f.extent.max_y + r);
        if !(grown.min_x <= grown.max_x && grown.min_y <= grown.max_y) {
            continue;
        }
        // Only the tiles of the scene's area: what lies past them isn't drawn.
        let columns = index(grown.min_x).max(*in_columns.start())..=index(grown.max_x).min(*in_columns.end());
        let rows = index(grown.min_y).max(*in_rows.start())..=index(grown.max_y).min(*in_rows.end());
        if columns.is_empty() || rows.is_empty() {
            continue;
        }
        let span = |r: &std::ops::RangeInclusive<i64>| r.end().saturating_sub(*r.start()).saturating_add(1);
        let count = span(&columns).saturating_mul(span(&rows));
        if usize::try_from(count).map_or(true, |n| n > MAX_TILES) {
            return None;
        }
        members = members.saturating_add(usize::try_from(count).unwrap_or(usize::MAX));
        if members > MAX_MEMBERS {
            return None;
        }
        for row in rows {
            for column in columns.clone() {
                tiles.entry(Tile { row, column }).or_default().push(k);
            }
        }
        if tiles.len() > MAX_TILES {
            return None;
        }
    }
    Some(tiles.into_iter().collect())
}

/// The finite runs of a ring or line: a point that isn't finite breaks it there, as
/// `decimate_runs` breaks it.
fn finite_runs(pts: &[Coord]) -> impl Iterator<Item = &[Coord]> {
    pts.split(|c| !c.is_finite()).filter(|r| !r.is_empty())
}

/// A ring clipped to `b` (Sutherland–Hodgman): the part inside it, closed along its edges;
/// empty when less than a triangle is left.
fn clip_ring(pts: &[Coord], b: &Extent) -> Vec<Coord> {
    let mut cur: Vec<Coord> = pts.to_vec();
    for edge in 0..4 {
        if cur.is_empty() {
            break;
        }
        let inside = |p: Coord| match edge {
            0 => p.x >= b.min_x,
            1 => p.x <= b.max_x,
            2 => p.y >= b.min_y,
            _ => p.y <= b.max_y,
        };
        let cross = |p: Coord, q: Coord| match edge {
            0 => Coord::new(b.min_x, p.y + (q.y - p.y) * (b.min_x - p.x) / (q.x - p.x)),
            1 => Coord::new(b.max_x, p.y + (q.y - p.y) * (b.max_x - p.x) / (q.x - p.x)),
            2 => Coord::new(p.x + (q.x - p.x) * (b.min_y - p.y) / (q.y - p.y), b.min_y),
            _ => Coord::new(p.x + (q.x - p.x) * (b.max_y - p.y) / (q.y - p.y), b.max_y),
        };
        let mut out = Vec::with_capacity(cur.len() + 4);
        let Some(&last) = cur.last() else { break };
        let mut prev = last;
        for &p in &cur {
            match (inside(prev), inside(p)) {
                (true, true) => out.push(p),
                (true, false) => out.push(cross(prev, p)),
                (false, true) => {
                    out.push(cross(prev, p));
                    out.push(p);
                }
                (false, false) => {}
            }
            prev = p;
        }
        cur = out;
    }
    if cur.len() < 3 { Vec::new() } else { cur }
}

/// The parts of a line inside `b`, each segment clipped by Liang–Barsky.
fn clip_line(pts: &[Coord], b: &Extent) -> Vec<Vec<Coord>> {
    let mut parts: Vec<Vec<Coord>> = Vec::new();
    let mut cur: Vec<Coord> = Vec::new();
    for w in pts.windows(2) {
        let (Some(&p), Some(&q)) = (w.first(), w.get(1)) else { continue };
        let (dx, dy) = (q.x - p.x, q.y - p.y);
        let (mut t0, mut t1) = (0.0f64, 1.0f64);
        let mut keep = true;
        for (num, den) in [(p.x - b.min_x, -dx), (b.max_x - p.x, dx), (p.y - b.min_y, -dy), (b.max_y - p.y, dy)] {
            if den == 0.0 {
                if num < 0.0 {
                    keep = false;
                    break;
                }
            } else {
                let t = num / den;
                if den < 0.0 {
                    t0 = t0.max(t);
                } else {
                    t1 = t1.min(t);
                }
            }
        }
        if !keep || t0 > t1 {
            if cur.len() > 1 {
                parts.push(std::mem::take(&mut cur));
            }
            cur.clear();
            continue;
        }
        let a = Coord::new(p.x + dx * t0, p.y + dy * t0);
        let z = Coord::new(p.x + dx * t1, p.y + dy * t1);
        if cur.is_empty() || t0 > 0.0 {
            if cur.len() > 1 {
                parts.push(std::mem::take(&mut cur));
            }
            cur = vec![a];
        }
        cur.push(z);
        if t1 < 1.0 {
            parts.push(std::mem::take(&mut cur));
        }
    }
    if cur.len() > 1 {
        parts.push(cur);
    }
    parts
}

/// A closed ring cut to `b`: whole (`Ok`) when it lies inside, else its open parts inside
/// (`Err`), the part through its first point joined across it so no cap lands inside the tile.
fn cut_ring(ring: &[Coord], b: &Extent) -> Result<Vec<Coord>, Vec<Vec<Coord>>> {
    let inside = |c: &Coord| c.x >= b.min_x && c.x <= b.max_x && c.y >= b.min_y && c.y <= b.max_y;
    if ring.iter().all(inside) {
        return Ok(ring.to_vec());
    }
    let (Some(&first), Some(&last)) = (ring.first(), ring.last()) else { return Err(Vec::new()) };
    let mut closed = ring.to_vec();
    if first != last {
        closed.push(first);
    }
    let mut parts = clip_line(&closed, b);
    // A ring starting inside the tile has its first part start, and its last end, at its first
    // point: they are one stretch of the outline.
    if inside(&first) && parts.len() > 1 {
        let starts = parts.first().and_then(|p| p.first()).copied() == Some(first);
        let ends = parts.last().and_then(|p| p.last()).copied() == Some(first);
        if starts && ends {
            let head = parts.remove(0);
            if let Some(tail) = parts.last_mut() {
                tail.extend(head.into_iter().skip(1));
            }
        }
    }
    Err(parts)
}

/// Appends one feature cut to the grown tile `b`, in the order `append_geometry` draws it.
fn append_cut(
    v: &mut Vec<MeshVertex>,
    i: &mut Vec<u32>,
    f: &Feature<'_>,
    b: &Extent,
    origin: [f64; 2],
    wireframe: bool,
    ppp: f32,
    map_per_px: f64,
    limit: usize,
) -> Result<(), TessError> {
    let s = &f.symbol;
    let room = |v: &Vec<MeshVertex>| limit.saturating_sub(v.len());
    // Outlines: rings inside the tile stroked closed, the rest as open parts with no cap floor.
    let outline = |v: &mut Vec<MeshVertex>, i: &mut Vec<u32>, polys: &[Vec<Vec<Coord>>], color, width: f32| {
        let (mut rings, mut parts) = (Vec::new(), Vec::new());
        for ring in polys.iter().flatten() {
            for run in finite_runs(ring) {
                match cut_ring(run, b) {
                    Ok(r) => rings.push(r),
                    Err(p) => parts.extend(p),
                }
            }
        }
        crate::mesh::extend_mesh(v, i, crate::mesh::stroke(&rings, origin, color, width, map_per_px, true, 3, room(v)))?;
        crate::mesh::extend_mesh(v, i, crate::mesh::stroke(&parts, origin, color, width, map_per_px, false, 2, room(v)))
    };
    match f.geometry {
        Geometry::Point(_) | Geometry::MultiPoint(_) => {}
        Geometry::Line(parts) => {
            if s.stroke.a() > 0 && (s.stroke_width * ppp).is_finite() {
                let cut: Vec<Vec<Coord>> = parts.iter().flat_map(|p| finite_runs(p).flat_map(|r| clip_line(r, b))).collect();
                let color = crate::mesh::premul(s.stroke);
                crate::mesh::extend_mesh(v, i, crate::mesh::stroke_lines(&cut, origin, color, s.stroke_width * ppp, map_per_px, room(v)))?;
            }
        }
        Geometry::Polygon(polys) if wireframe => {
            outline(v, i, polys, crate::mesh::premul(s.stroke.with_alpha(255)), s.stroke_width.max(1.0) * ppp)?;
        }
        Geometry::Polygon(polys) => {
            if s.fill.a() > 0 {
                let clipped: Vec<Vec<Vec<Coord>>> = polys
                    .iter()
                    .map(|rings| rings.iter().flat_map(|r| finite_runs(r).map(|run| clip_ring(run, b))).filter(|r| !r.is_empty()).collect())
                    .filter(|rings: &Vec<Vec<Coord>>| !rings.is_empty())
                    .collect();
                let color = crate::mesh::premul(s.fill);
                crate::mesh::extend_mesh(v, i, crate::mesh::fill_polygon(&clipped, origin, color, map_per_px, room(v)))?;
            }
            if s.stroke_width > 0.0 && s.stroke.a() > 0 {
                outline(v, i, polys, crate::mesh::premul(s.stroke), s.stroke_width * ppp)?;
            }
        }
    }
    Ok(())
}

/// One tile's mesh: its features in layer order, each whole or cut to the tile grown by its
/// reach. `Ok(None)` when nothing draws there.
fn tile_mesh(
    features: &[Feature<'_>],
    tile: Tile,
    members: &[usize],
    side: f64,
    wireframe: bool,
    ppp: f32,
    map_per_px: f64,
    limit: usize,
    used: &AtomicUsize,
) -> Result<MadeTile, TessError> {
    let origin = tile.corner(side);
    let square = Extent::new(origin[0], origin[1], origin[0] + side, origin[1] + side);
    let (mut v, mut i, mut no_points) = (Vec::new(), Vec::new(), Vec::new());
    let mut need = 0usize;
    for &k in members {
        let Some(f) = features.get(k) else { continue };
        // Tiles finished already past the limit: the layer is refused whatever this one holds,
        // so it stops early. A tile's own room is the whole limit, so whether the layer meshes
        // doesn't hang on which tiles finished first (the same on any number of threads).
        if used.load(Ordering::Relaxed) > limit {
            return Err(TessError::TooDense);
        }
        let r = f.reach;
        let grown = Extent::new(square.min_x - r, square.min_y - r, square.max_x + r, square.max_y + r);
        let e = &f.extent;
        if e.min_x >= grown.min_x && e.max_x <= grown.max_x && e.min_y >= grown.min_y && e.max_y <= grown.max_y {
            // Whole, it takes about its estimate; cut, only its part here, bounded as it's made.
            need = need.max(v.len().saturating_add(f.estimate));
            if need > limit {
                return Err(TessError::TooDense);
            }
            crate::mesh::append_geometry(&mut v, &mut i, &mut no_points, f.geometry, &f.symbol, origin, wireframe, ppp, map_per_px, limit)?;
        } else {
            append_cut(&mut v, &mut i, f, &grown, origin, wireframe, ppp, map_per_px, limit)?;
        }
    }
    // Each part is refused once it passes the room left: it fits a limit as low as the vertices.
    let need = need.max(v.len());
    if i.is_empty() {
        return Ok(MadeTile { tile, need, cmd: None });
    }
    if used.fetch_add(v.len(), Ordering::Relaxed).saturating_add(v.len()) > limit {
        return Err(TessError::TooDense);
    }
    let square = [0.0, 0.0, side as f32, side as f32];
    let cmd = DrawCmd::Mesh { origin, vertices: Arc::new(v), indices: Arc::new(i), square: Some(square) };
    Ok(MadeTile { tile, need, cmd: Some(cmd) })
}

/// The layer's features (points left out: they aren't tiled) as tile meshes within `area`,
/// tessellated on several threads; the tiles in `kept` are taken as they are. `None` when the
/// scale has no tiles or the area would take too many (the caller makes one mesh); else the
/// tiles made, in grid order, and their vertices, or why the layer can't be meshed.
pub(crate) fn tile_meshes(
    features: &[Feature<'_>],
    area: &Extent,
    kept: &[MadeTile],
    wireframe: bool,
    ppp: f32,
    map_per_px: f64,
    limit: usize,
) -> Option<Result<(Vec<MadeTile>, usize), TessError>> {
    tile_meshes_on(features, area, kept, wireframe, ppp, map_per_px, limit, None)
}

/// [`tile_meshes`] on `threads` threads, or as [`threads_for`] decides.
#[expect(clippy::too_many_arguments, reason = "the layer, the scene's area and scale, the tiles kept and the threads")]
fn tile_meshes_on(
    features: &[Feature<'_>],
    area: &Extent,
    kept: &[MadeTile],
    wireframe: bool,
    ppp: f32,
    map_per_px: f64,
    limit: usize,
    threads: Option<usize>,
) -> Option<Result<(Vec<MadeTile>, usize), TessError>> {
    let side = tile_side(map_per_px)?;
    let tiles = buckets(features, area, side)?;
    // The tiles kept from the last scene, as meshing them again would make them.
    let taken: Vec<Option<&MadeTile>> =
        tiles.iter().map(|(tile, _)| kept.binary_search_by(|k| k.tile.cmp(tile)).ok().and_then(|i| kept.get(i))).collect();
    let to_make: Vec<&(Tile, Vec<usize>)> = tiles.iter().zip(&taken).filter(|(_, k)| k.is_none()).map(|(t, _)| t).collect();
    let threads = threads.unwrap_or_else(|| threads_for(features, &to_make));
    // The vertices of the tiles made or taken so far: past `limit` the layer is refused, and the
    // tiles still at work stop at their next feature, so it never holds much more than `limit`
    // (by the tiles at work at once, each at most `limit`).
    let used = AtomicUsize::new(taken.iter().flatten().map(|k| k.vertices()).fold(0, usize::saturating_add));
    if used.load(Ordering::Relaxed) > limit {
        return Some(Err(TessError::TooDense));
    }
    let made = crate::tile_mesh::parallel_on(&to_make, threads, |(tile, members)| {
        tile_mesh(features, *tile, members, side, wireframe, ppp, map_per_px, limit, &used)
    });
    let mut made = made.into_iter();
    let mut out = Vec::with_capacity(tiles.len());
    let mut used = 0usize;
    for k in taken {
        let tile = match k {
            // One that wouldn't fit the limit would be refused again, and with it the layer, in
            // its place in grid order (an error in a tile before it is the layer's).
            Some(k) if k.need > limit => return Some(Err(TessError::TooDense)),
            Some(k) => k.share(),
            // A tile that panicked (hostile data) leaves the layer to the CPU, as lyon refusing it.
            None => match made.next().flatten().unwrap_or(Err(TessError::Refused)) {
                Ok(t) => t,
                Err(e) => return Some(Err(e)),
            },
        };
        used = used.saturating_add(tile.vertices());
        if used > limit {
            return Some(Err(TessError::TooDense));
        }
        out.push(tile);
    }
    Some(Ok((out, used)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tile_is_the_power_of_two_nearest_its_pixels() {
        assert_eq!(tile_side(1.0), Some(512.0));
        assert_eq!(tile_side(0.75), Some(512.0));
        assert_eq!(tile_side(3.0), Some(2048.0));
        assert_eq!(tile_side(0.0), None);
        assert_eq!(tile_side(f64::NAN), None);
    }

    #[test]
    fn a_ring_starting_inside_is_cut_into_parts_joined_through_its_first_point() {
        let b = Extent::new(0.0, 0.0, 10.0, 10.0);
        let ring = [Coord::new(5.0, 5.0), Coord::new(20.0, 5.0), Coord::new(20.0, 8.0), Coord::new(5.0, 8.0)];
        let Err(parts) = cut_ring(&ring, &b) else { panic!("cut") };
        assert_eq!(parts.len(), 1, "{parts:?}");
        assert_eq!(parts[0], vec![Coord::new(10.0, 8.0), Coord::new(5.0, 8.0), Coord::new(5.0, 5.0), Coord::new(10.0, 5.0)]);
        assert_eq!(cut_ring(&ring[..1], &b), Ok(ring[..1].to_vec()));
    }

    /// A symbol with a stroke `width` points wide in `stroke` and a fill in `fill`.
    fn symbol(fill: rustgis_map::Rgba, width: f32) -> Symbol {
        Symbol {
            fill,
            stroke: rustgis_map::Rgba::rgb(0, 0, 0),
            stroke_width: width,
            ..Symbol::default_for(rustgis_data::GeometryType::Polygon, 0)
        }
    }

    fn feature(g: &Geometry, s: Symbol) -> Feature<'_> {
        // As the layer's features reach: half the stroke as tessellated and a pixel, none unstroked.
        let reach = if s.stroke_width > 0.0 { f64::from(crate::mesh::tessellation_width(s.stroke_width, 1.0)) / 2.0 + 2.0 } else { 0.0 };
        let estimate = crate::mesh::estimate_vertices(g, &s, false);
        Feature { geometry: g, symbol: s, estimate, extent: g.extent(), reach }
    }

    /// The tiles' Mesh commands.
    fn commands(tiles: &[MadeTile]) -> Vec<DrawCmd> {
        tiles.iter().filter_map(|t| t.share().cmd).collect()
    }

    /// Each tile's mesh, as (origin, its vertices' positions from the map's origin).
    fn tiles(cmds: &[DrawCmd]) -> Vec<([f64; 2], Vec<[f64; 2]>)> {
        cmds.iter()
            .filter_map(|c| match c {
                DrawCmd::Mesh { origin, vertices, square: Some(_), .. } => Some((
                    *origin,
                    vertices.iter().map(|v| [origin[0] + f64::from(v.position[0]), origin[1] + f64::from(v.position[1])]).collect(),
                )),
                _ => None,
            })
            .collect()
    }

    /// Lines and polygons over many tiles, meshed on one thread and on eight: the same commands,
    /// vertex for vertex, in the same order.
    #[test]
    fn a_layers_tiles_are_the_same_on_any_number_of_threads() {
        let geoms: Vec<Geometry> = (0u32..40)
            .map(|n| {
                let k = f64::from(n);
                if n.is_multiple_of(2) {
                    Geometry::Line(vec![vec![
                        Coord::new(-900.0 + k * 13.0, -700.0),
                        Coord::new(800.0, 600.0 - k * 17.0),
                        Coord::new(-300.0, 900.0),
                    ]])
                } else {
                    let (x, y) = (-1000.0 + k * 40.0, -800.0 + k * 31.0);
                    Geometry::Polygon(vec![vec![vec![
                        Coord::new(x, y),
                        Coord::new(x + 700.0, y + 50.0),
                        Coord::new(x + 600.0, y + 650.0),
                        Coord::new(x - 20.0, y + 500.0),
                    ]]])
                }
            })
            .collect();
        let features: Vec<Feature<'_>> = geoms.iter().map(|g| feature(g, symbol(rustgis_map::Rgba::rgb(9, 99, 199), 3.0))).collect();
        let area = Extent::new(-2048.0, -2048.0, 2048.0, 2048.0);
        let made = |threads| {
            let (cmds, used) = tile_meshes_on(&features, &area, &[], false, 1.0, 1.0, usize::MAX, Some(threads)).unwrap().unwrap();
            let cmds = commands(&cmds);
            let bytes: Vec<Vec<u8>> = cmds
                .iter()
                .map(|c| match c {
                    DrawCmd::Mesh { origin, vertices, indices, square } => [
                        bytemuck::cast_slice::<_, u8>(origin),
                        bytemuck::cast_slice(vertices.as_slice()),
                        bytemuck::cast_slice(indices.as_slice()),
                        bytemuck::cast_slice(square.as_slice()),
                    ]
                    .concat(),
                    _ => Vec::new(),
                })
                .collect();
            (bytes, used)
        };
        let one = made(1);
        assert!(one.0.len() > 4, "many tiles: {}", one.0.len());
        assert_eq!(made(8), one);
    }

    /// A long line over many tiles, its whole estimate past the layer's limit: each tile takes
    /// only its part, so the layer meshes; the tiles together past the limit give the layer up.
    #[test]
    fn a_cut_feature_takes_only_its_part_and_the_layers_tiles_share_its_limit() {
        let pts: Vec<Coord> = (0..4000).map(|k| Coord::new(-2000.0 + f64::from(k), 30.0 + f64::from(k % 7))).collect();
        let line = Geometry::Line(vec![pts]);
        let f = feature(&line, symbol(rustgis_map::Rgba::rgb(0, 0, 0), 2.0));
        let area = Extent::new(-1024.0, -1024.0, 1024.0, 1024.0);
        let (all, used) = tile_meshes_on(std::slice::from_ref(&f), &area, &[], false, 1.0, 1.0, usize::MAX, Some(1)).unwrap().unwrap();
        let all = commands(&all);
        assert!(tiles(&all).len() >= 4, "{}", tiles(&all).len());
        assert!(used < f.estimate, "cut to the area: {used} of {}", f.estimate);
        let biggest = tiles(&all).iter().map(|(_, v)| v.len()).max().unwrap_or(0);
        // Room for the biggest tile but not the whole estimate: it meshes.
        let room = used.max(biggest);
        assert!(room < f.estimate);
        assert!(tile_meshes_on(std::slice::from_ref(&f), &area, &[], false, 1.0, 1.0, room, Some(1)).unwrap().is_ok());
        // Past the limit by a vertex, or with room for each tile but not all: refused alike on
        // any number of threads.
        for limit in [used - 1, biggest.max(used / 2)] {
            for threads in [1, 8] {
                let short = tile_meshes_on(std::slice::from_ref(&f), &area, &[], false, 1.0, 1.0, limit, Some(threads)).unwrap();
                assert!(matches!(short, Err(TessError::TooDense)), "limit {limit}, {threads} threads");
            }
        }
    }

    /// A line just west of a tile's east edge, with a stroke wider than the gap: the next tile
    /// east draws what spills over (its mesh reaches past its west edge), and a polygon larger
    /// than the tiles is filled in each only within it.
    #[test]
    fn a_stroke_spills_into_the_next_tile_and_a_fill_keeps_to_its_own() {
        let line = Geometry::Line(vec![vec![Coord::new(510.0, 100.0), Coord::new(510.0, 300.0)]]);
        let wide = feature(&line, symbol(rustgis_map::Rgba::rgb(0, 0, 0), 8.0));
        let area = Extent::new(-2048.0, -2048.0, 2048.0, 2048.0);
        let (cmds, _) = tile_meshes(std::slice::from_ref(&wide), &area, &[], false, 1.0, 1.0, usize::MAX).unwrap().unwrap();
        let cmds = commands(&cmds);
        let origins: Vec<[f64; 2]> = tiles(&cmds).into_iter().map(|(o, _)| o).collect();
        assert_eq!(origins, [[0.0, 0.0], [512.0, 0.0]], "its own tile and the next east");
        let square = Geometry::Polygon(vec![vec![vec![
            Coord::new(-300.0, -300.0),
            Coord::new(900.0, -300.0),
            Coord::new(900.0, 900.0),
            Coord::new(-300.0, 900.0),
        ]]]);
        let filled = feature(&square, Symbol { stroke_width: 0.0, ..symbol(rustgis_map::Rgba::rgb(9, 99, 199), 0.0) });
        let (cmds, _) = tile_meshes(std::slice::from_ref(&filled), &area, &[], false, 1.0, 1.0, usize::MAX).unwrap().unwrap();
        let cmds = commands(&cmds);
        let tiled = tiles(&cmds);
        assert_eq!(tiled.len(), 9, "the nine tiles it covers");
        for (origin, positions) in tiled {
            for [x, y] in positions {
                assert!(x >= origin[0] && x <= origin[0] + 512.0 && y >= origin[1] && y <= origin[1] + 512.0, "{x},{y} in {origin:?}");
            }
        }
    }

    /// The tiles' commands as bytes: origin, vertices, indices and square.
    fn bytes(tiles: &[MadeTile]) -> Vec<Vec<u8>> {
        commands(tiles)
            .iter()
            .map(|c| match c {
                DrawCmd::Mesh { origin, vertices, indices, square } => [
                    bytemuck::cast_slice::<_, u8>(origin),
                    bytemuck::cast_slice(vertices.as_slice()),
                    bytemuck::cast_slice(indices.as_slice()),
                    bytemuck::cast_slice(square.as_slice()),
                ]
                .concat(),
                _ => Vec::new(),
            })
            .collect()
    }

    /// The scene panned: the tiles kept from before that are still in it are taken as they are
    /// (the same meshes), the rest made, and the whole is what meshing it afresh makes, vertex
    /// for vertex, a stroke spilling in from a feature past the new area's edge included.
    #[test]
    fn a_panned_scene_takes_its_kept_tiles_and_is_the_one_made_afresh() {
        let geoms: Vec<Geometry> = (0u32..60)
            .map(|n| {
                let k = f64::from(n);
                if n.is_multiple_of(2) {
                    Geometry::Line(vec![vec![Coord::new(-2500.0 + k * 83.0, -1900.0), Coord::new(2600.0, 1700.0 - k * 57.0)]])
                } else {
                    let (x, y) = (-2600.0 + k * 85.0, -2000.0 + k * 61.0);
                    Geometry::Polygon(vec![vec![vec![Coord::new(x, y), Coord::new(x + 300.0, y + 40.0), Coord::new(x + 260.0, y + 330.0)]]])
                }
            })
            .collect();
        // A line just past the panned area's east edge, its stroke reaching into its last tile.
        let edge = Geometry::Line(vec![vec![Coord::new(1540.0, -300.0), Coord::new(1540.0, 300.0)]]);
        let mut features: Vec<Feature<'_>> = geoms.iter().map(|g| feature(g, symbol(rustgis_map::Rgba::rgb(9, 99, 199), 3.0))).collect();
        features.push(feature(&edge, symbol(rustgis_map::Rgba::rgb(0, 0, 0), 12.0)));
        let before = Extent::new(-1800.0, -1500.0, 700.0, 900.0);
        let after = Extent::new(-1000.0, -1100.0, 1535.0, 1300.0);
        let made = |area: &Extent, kept: &[MadeTile]| {
            tile_meshes_on(&features, area, kept, false, 1.0, 1.0, usize::MAX, Some(1)).unwrap().unwrap()
        };
        let (kept, _) = made(&before, &[]);
        let (fresh, fresh_used) = made(&after, &[]);
        let (panned, panned_used) = made(&after, &kept);
        assert_eq!(bytes(&panned), bytes(&fresh));
        assert_eq!(panned_used, fresh_used);
        let mesh = |t: &MadeTile| match &t.cmd {
            Some(DrawCmd::Mesh { vertices, .. }) => Some(Arc::as_ptr(vertices)),
            _ => None,
        };
        let taken = panned.iter().filter(|t| mesh(t).is_some_and(|m| kept.iter().any(|k| mesh(k) == Some(m)))).count();
        let both = panned.iter().filter(|t| t.cmd.is_some() && kept.iter().any(|k| k.tile == t.tile)).count();
        assert!(both >= 4, "tiles in both scenes: {both}");
        assert_eq!(taken, both, "every tile in both scenes is taken, not made again");
        // The stroke past the edge reaches the panned area's last column, from outside it.
        let east = panned.iter().map(|t| t.tile.column).max();
        assert_eq!(east, Some(2));
        // A kept tile too big for the limit refuses the layer, as meshing it again would.
        let biggest = kept.iter().map(|t| t.need).max().unwrap_or(0);
        for kept in [&kept[..], &[]] {
            assert!(matches!(made_within(&features, &after, kept, biggest - 1), Err(TessError::TooDense)));
        }
    }

    fn made_within(features: &[Feature<'_>], area: &Extent, kept: &[MadeTile], limit: usize) -> Result<(Vec<MadeTile>, usize), TessError> {
        tile_meshes_on(features, area, kept, false, 1.0, 1.0, limit, Some(1)).unwrap()
    }

    #[test]
    fn a_feature_reaches_the_tiles_its_stroke_reaches() {
        let g = Geometry::Line(vec![vec![Coord::new(1.0, 1.0), Coord::new(511.0, 1.0)]]);
        let f = |reach| Feature {
            geometry: &g,
            symbol: Symbol::default_for(rustgis_data::GeometryType::Polyline, 0),
            estimate: 0,
            extent: g.extent(),
            reach,
        };
        let area = Extent::new(-4096.0, -4096.0, 4096.0, 4096.0);
        let tiles = |reach| buckets(&[f(reach)], &area, 512.0).unwrap().into_iter().map(|(t, _)| (t.column, t.row)).collect::<Vec<_>>();
        assert_eq!(tiles(0.0), [(0, 0)]);
        assert_eq!(tiles(2.0), [(-1, -1), (0, -1), (1, -1), (-1, 0), (0, 0), (1, 0)]);
    }
}
