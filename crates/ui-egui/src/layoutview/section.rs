//! A map frame's section lines on the page (#2863): where each is drawn, which one the pointer is
//! on, and where a dragged one lands.

use super::*;
use egui::{CursorIcon, Pos2, Rect};
use rustgis_engine::geom::Coord;
use rustgis_engine::map::layout::{Element, FrameShape, Layout, Section, SectionLine};
use rustgis_engine::map::{Camera, Map};
use rustgis_engine::render::layout::{SectionEnds, section_ends, section_moved};

/// The map a frame shows and the camera it is drawn through, when the project has it.
pub(super) fn frame_map<'a>(app: &'a RustGisApp, e: &Element) -> Option<(&'a Map, Camera)> {
    let ElementKind::MapFrame { map, center, scale, bearing, pitch, .. } = &e.kind else { return None };
    let m = app.session.project.maps.iter().find(|m| m.name.eq_ignore_ascii_case(map))?;
    Some((m, frame_camera(m, *center, *scale, *bearing, *pitch)))
}

/// A section line's ends (points of its frame `e`) on screen.
pub(super) fn section_on_screen(e: &Element, page: Rect, s: f32, ends: SectionEnds) -> [Pos2; 2] {
    ends.map(|(x, y)| to_screen(page, s, (e.x + x / PT_PER_MM, e.y + y / PT_PER_MM)))
}

/// The section line (#2863) of the selected map frame under a screen point, when it can be dragged:
/// the frame, the line's index and whether it is vertical. Not one of a locked frame, nor one under
/// another element, nor when a drag pans (map frames being navigated, or Shift held).
pub(super) fn section_at(app: &RustGisApp, layout: &Layout, page: Rect, s: f32, pos: Pos2, pans: bool) -> Option<(u32, usize, bool)> {
    let e = app.layout.selected.and_then(|id| layout.element(id)).filter(|e| !e.locked && !pans)?;
    let ElementKind::MapFrame { shape, sections, .. } = &e.kind else { return None };
    let at = to_mm(page, s, pos);
    if sections.is_empty() || layout.hit(at.0, at.1) != Some(e.id) {
        return None;
    }
    let (map, cam) = frame_map(app, e)?;
    let size = (e.w * PT_PER_MM, e.h * PT_PER_MM);
    let under = |(i, l): (usize, &Section)| {
        let ends = section_ends(map, &cam, size, shape, l.line).ok()?;
        let [a, b] = section_on_screen(e, page, s, ends);
        let reach = GUIDE_PICK.max((l.width / PT_PER_MM) as f32 * s / 2.0);
        let d = Rect::from_two_pos(a, b).distance_to_pos(pos);
        (d < reach).then_some((d, i, matches!(l.line, SectionLine::Vertical(_))))
    };
    // The nearest; the one drawn last where two cross.
    sections.iter().enumerate().rev().filter_map(under).min_by(|a, b| a.0.total_cmp(&b.0)).map(|(_, i, vertical)| (e.id, i, vertical))
}

/// A section line as a drag takes it: its place, and a key of its name (a frame's only one of that
/// name), so that one put at the same place meanwhile isn't taken for it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Taken {
    line: SectionLine,
    name: u64,
}

impl Taken {
    fn of(l: &Section) -> Self {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let mut h = DefaultHasher::new();
        l.name.trim().to_lowercase().hash(&mut h);
        Taken { line: l.line, name: h.finish() }
    }
}

/// Section line `i` of frame `id`, with them, as a drag takes it.
pub(super) fn section_line(layout: &Layout, id: u32, i: usize) -> Option<(u32, usize, Taken)> {
    let ElementKind::MapFrame { sections, .. } = &layout.element(id)?.kind else { return None };
    Some((id, i, Taken::of(sections.get(i)?)))
}

/// Whether section line `i` of frame `id` is vertical.
pub(super) fn section_vertical(layout: &Layout, id: u32, i: usize) -> bool {
    matches!(layout.element(id).map(|e| &e.kind), Some(ElementKind::MapFrame { sections, .. })
        if sections.get(i).is_some_and(|l| matches!(l.line, SectionLine::Vertical(_))))
}

/// The cursor over a section line that can be dragged.
pub(super) fn section_cursor(vertical: bool) -> CursorIcon {
    if vertical { CursorIcon::ResizeHorizontal } else { CursorIcon::ResizeVertical }
}

/// Whether page point `at` (mm) is on frame `e`, within its `shape` (a circle as wide as the
/// shorter side, as it is drawn), counted as the shape is filled: by its winding number.
fn in_shape(e: &Element, shape: &FrameShape, at: (f64, f64)) -> bool {
    let (x, y) = e.unturned(at.0, at.1);
    let (x, y) = (x - e.x, y - e.y);
    let side = e.w.min(e.h);
    let (bw, bh) = if *shape == FrameShape::Circle { (side, side) } else { (e.w, e.h) };
    let outline: Vec<(f64, f64)> =
        shape.outline().into_iter().map(|(u, v)| ((e.w - bw) / 2.0 + u * bw, (e.h - bh) / 2.0 + v * bh)).collect();
    let winding: i32 = outline
        .iter()
        .zip(outline.iter().cycle().skip(1))
        .filter(|(a, b)| (a.1 <= y) != (b.1 <= y) && a.0 + (y - a.1) / (b.1 - a.1) * (b.0 - a.0) <= x)
        .map(|(a, _)| if a.1 <= y { 1 } else { -1 })
        .sum();
    winding != 0
}

/// Where a section line being dragged (its frame, its index, the line taken and the drag's start)
/// lands with the pointer at `at` (mm): the frame, the line's name, its place, and its ends in the
/// frame's points; `None` off the frame, or where the frame would not draw it.
pub(super) fn section_landing<'a>(
    app: &RustGisApp,
    layout: &'a Layout,
    (id, i, taken, start): (u32, usize, Taken, (f64, f64)),
    at: (f64, f64),
) -> Option<(&'a Element, &'a str, Coord, SectionEnds)> {
    let e = layout.element(id)?;
    let ElementKind::MapFrame { shape, sections, .. } = &e.kind else { return None };
    // The line taken, still at its index: one removed, moved, renamed or replaced meanwhile isn't
    // moved. Dropped off the frame or outside its shape, it stays.
    let l = sections.get(i).filter(|l| Taken::of(l) == taken)?;
    if !in_shape(e, shape, at) {
        return None;
    }
    let (map, cam) = frame_map(app, e)?;
    let size = (e.w * PT_PER_MM, e.h * PT_PER_MM);
    let (by, along) = match l.line {
        SectionLine::Horizontal(_) => (at.1 - start.1, at.0 - e.x),
        SectionLine::Vertical(_) => (at.0 - start.0, at.1 - e.y),
    };
    let (by, along) = (by * PT_PER_MM, along * PT_PER_MM);
    let (line, ends) = section_moved(map, &cam, size, shape, l.line, by, along)?;
    Some((e, &l.name, line.at(), ends))
}
