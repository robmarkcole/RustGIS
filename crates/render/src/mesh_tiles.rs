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

/// The threads to mesh `tiles` on: one for a small layer, else as many as the tiles and the user's
/// thread count allow, and no more than [`rustgis_data::memory::thread_scratch`] holds beyond one
/// thread's, each meshing a tile as big as the biggest (by the features' estimates, each counted
/// whole in every tile it reaches).
fn threads_for(features: &[Feature<'_>], tiles: &[(Tile, Vec<usize>)]) -> usize {
    let estimate =
        |members: &[usize]| members.iter().filter_map(|k| features.get(*k)).map(|f| f.estimate).fold(0usize, usize::saturating_add);
    let total = features.iter().map(|f| f.estimate).fold(0usize, usize::saturating_add);
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
struct Tile {
    row: i64,
    column: i64,
}

impl Tile {
    /// Its south-west corner, map units: its mesh's origin.
    fn corner(self, side: f64) -> [f64; 2] {
        [self.column as f64 * side, self.row as f64 * side]
    }
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

/// The tiles of a layer's features within `area`, each with the features (by index) reaching
/// it, in grid order. `None` when they would be more than [`MAX_TILES`], or list more than
/// [`MAX_MEMBERS`] features in all.
fn buckets(features: &[Feature<'_>], area: &Extent, side: f64) -> Option<Vec<(Tile, Vec<usize>)>> {
    let index = |v: f64| (v / side).floor() as i64;
    let mut tiles: BTreeMap<Tile, Vec<usize>> = BTreeMap::new();
    let mut members = 0usize;
    for (k, f) in features.iter().enumerate() {
        let r = f.reach;
        let grown = Extent::new(f.extent.min_x - r, f.extent.min_y - r, f.extent.max_x + r, f.extent.max_y + r);
        // Only the tiles of the scene's area: what lies past it isn't drawn.
        let e =
            Extent::new(grown.min_x.max(area.min_x), grown.min_y.max(area.min_y), grown.max_x.min(area.max_x), grown.max_y.min(area.max_y));
        if !(e.min_x <= e.max_x && e.min_y <= e.max_y) {
            continue;
        }
        let (columns, rows) = (index(e.min_x)..=index(e.max_x), index(e.min_y)..=index(e.max_y));
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
) -> Result<Option<DrawCmd>, TessError> {
    let origin = tile.corner(side);
    let square = Extent::new(origin[0], origin[1], origin[0] + side, origin[1] + side);
    let (mut v, mut i, mut no_points) = (Vec::new(), Vec::new(), Vec::new());
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
            if v.len().saturating_add(f.estimate) > limit {
                return Err(TessError::TooDense);
            }
            crate::mesh::append_geometry(&mut v, &mut i, &mut no_points, f.geometry, &f.symbol, origin, wireframe, ppp, map_per_px, limit)?;
        } else {
            append_cut(&mut v, &mut i, f, &grown, origin, wireframe, ppp, map_per_px, limit)?;
        }
    }
    if i.is_empty() {
        return Ok(None);
    }
    if used.fetch_add(v.len(), Ordering::Relaxed).saturating_add(v.len()) > limit {
        return Err(TessError::TooDense);
    }
    let square = [0.0, 0.0, side as f32, side as f32];
    Ok(Some(DrawCmd::Mesh { origin, vertices: Arc::new(v), indices: Arc::new(i), square: Some(square) }))
}

/// The layer's features (points left out: they aren't tiled) as tile meshes within `area`,
/// tessellated on several threads. `None` when the scale has no tiles or the area would take
/// too many (the caller makes one mesh); else the tiles' commands and their vertices, or why
/// the layer can't be meshed.
pub(crate) fn tile_meshes(
    features: &[Feature<'_>],
    area: &Extent,
    wireframe: bool,
    ppp: f32,
    map_per_px: f64,
    limit: usize,
) -> Option<Result<(Vec<DrawCmd>, usize), TessError>> {
    tile_meshes_on(features, area, wireframe, ppp, map_per_px, limit, None)
}

/// [`tile_meshes`] on `threads` threads, or as [`threads_for`] decides.
fn tile_meshes_on(
    features: &[Feature<'_>],
    area: &Extent,
    wireframe: bool,
    ppp: f32,
    map_per_px: f64,
    limit: usize,
    threads: Option<usize>,
) -> Option<Result<(Vec<DrawCmd>, usize), TessError>> {
    let side = tile_side(map_per_px)?;
    let tiles = buckets(features, area, side)?;
    let threads = threads.unwrap_or_else(|| threads_for(features, &tiles));
    // The vertices of the tiles made so far: past `limit` the layer is refused, and the tiles
    // still at work stop at their next feature, so it never holds much more than `limit` (by
    // the tiles at work at once, each at most `limit`).
    let used = AtomicUsize::new(0);
    let made = crate::tile_mesh::parallel_on(&tiles, threads, |(tile, members)| {
        tile_mesh(features, *tile, members, side, wireframe, ppp, map_per_px, limit, &used)
    });
    let mut cmds = Vec::with_capacity(made.len());
    let mut used = 0usize;
    for m in made {
        // A tile that panicked (hostile data) leaves the layer to the CPU, as lyon refusing it.
        match m.unwrap_or(Err(TessError::Refused)) {
            Ok(Some(cmd)) => {
                if let DrawCmd::Mesh { vertices, .. } = &cmd {
                    used = used.saturating_add(vertices.len());
                }
                cmds.push(cmd);
            }
            Ok(None) => {}
            Err(e) => return Some(Err(e)),
        }
        if used > limit {
            return Some(Err(TessError::TooDense));
        }
    }
    Some(Ok((cmds, used)))
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
            let (cmds, used) = tile_meshes_on(&features, &area, false, 1.0, 1.0, usize::MAX, Some(threads)).unwrap().unwrap();
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
        let (all, used) = tile_meshes_on(std::slice::from_ref(&f), &area, false, 1.0, 1.0, usize::MAX, Some(1)).unwrap().unwrap();
        assert!(tiles(&all).len() >= 4, "{}", tiles(&all).len());
        assert!(used < f.estimate, "cut to the area: {used} of {}", f.estimate);
        let biggest = tiles(&all).iter().map(|(_, v)| v.len()).max().unwrap_or(0);
        // Room for the biggest tile but not the whole estimate: it meshes.
        let room = used.max(biggest);
        assert!(room < f.estimate);
        assert!(tile_meshes_on(std::slice::from_ref(&f), &area, false, 1.0, 1.0, room, Some(1)).unwrap().is_ok());
        // Past the limit by a vertex, or with room for each tile but not all: refused alike on
        // any number of threads.
        for limit in [used - 1, biggest.max(used / 2)] {
            for threads in [1, 8] {
                let short = tile_meshes_on(std::slice::from_ref(&f), &area, false, 1.0, 1.0, limit, Some(threads)).unwrap();
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
        let (cmds, _) = tile_meshes(std::slice::from_ref(&wide), &area, false, 1.0, 1.0, usize::MAX).unwrap().unwrap();
        let origins: Vec<[f64; 2]> = tiles(&cmds).into_iter().map(|(o, _)| o).collect();
        assert_eq!(origins, [[0.0, 0.0], [512.0, 0.0]], "its own tile and the next east");
        let square = Geometry::Polygon(vec![vec![vec![
            Coord::new(-300.0, -300.0),
            Coord::new(900.0, -300.0),
            Coord::new(900.0, 900.0),
            Coord::new(-300.0, 900.0),
        ]]]);
        let filled = feature(&square, Symbol { stroke_width: 0.0, ..symbol(rustgis_map::Rgba::rgb(9, 99, 199), 0.0) });
        let (cmds, _) = tile_meshes(std::slice::from_ref(&filled), &area, false, 1.0, 1.0, usize::MAX).unwrap().unwrap();
        let tiled = tiles(&cmds);
        assert_eq!(tiled.len(), 9, "the nine tiles it covers");
        for (origin, positions) in tiled {
            for [x, y] in positions {
                assert!(x >= origin[0] && x <= origin[0] + 512.0 && y >= origin[1] && y <= origin[1] + 512.0, "{x},{y} in {origin:?}");
            }
        }
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
