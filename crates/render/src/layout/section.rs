//! A map frame's section lines (#2863): where each runs across its frame, what isn't drawn and
//! why, and the lines with their names.

use super::*;

/// A section line's ends in its frame's points, from its left or top.
pub type SectionEnds = [(f64, f64); 2];

/// The globe a frame `w` × `h` points shows, if its map is on one.
fn section_globe(map: &Map, cam: &rustgis_map::Camera, (w, h): (f64, f64)) -> Option<rustgis_map::globe::Globe> {
    map.projection.is_globe().then(|| rustgis_map::globe::Globe::new(cam, w, h))
}

/// Where the place `at` is on a frame `w` × `h` points, in its points; on a map whose world
/// repeats, the copy nearest the frame's middle. `None` where the frame's map can't show it.
fn section_px(
    map: &Map,
    cam: &rustgis_map::Camera,
    globe: Option<&rustgis_map::globe::Globe>,
    (w, h): (f64, f64),
    mut at: Coord,
) -> Option<(f64, f64)> {
    if let Some(mid) = map.to_lonlat(cam.center).filter(|c| c.x.is_finite()) {
        at.x += 360.0 * ((mid.x - at.x) / 360.0).round();
    }
    match globe {
        Some(g) => g.forward(at),
        // A projection that holds a place to its reach (Web Mercator past 85.05°) gives another
        // place, back: that one isn't the frame's to show.
        None => {
            map.to_map(at).filter(|&m| map.to_lonlat(m).is_none_or(|back| same_place(back, at))).and_then(|m| cam.try_to_screen(m, w, h))
        }
    }
    .filter(|(x, y)| x.is_finite() && y.is_finite())
}

/// `line` moved `by` points across itself (down, or right for a vertical one) on a frame drawn as
/// for [`section_ends`], and its ends there. Its place keeps where it is along the line; where that
/// isn't in the frame's shape there, it goes to `along` points from the frame's left (or top).
/// `None` where the frame would not draw the line there.
pub fn section_moved(
    map: &Map,
    cam: &rustgis_map::Camera,
    (w, h): (f64, f64),
    shape: &FrameShape,
    line: SectionLine,
    by: f64,
    along: f64,
) -> Option<(SectionLine, SectionEnds)> {
    let globe = section_globe(map, cam, (w, h));
    let vertical = matches!(line, SectionLine::Vertical(_));
    let swap = |(x, y): (f64, f64)| if vertical { (y, x) } else { (x, y) };
    let (own, across) = swap(section_px(map, cam, globe.as_ref(), (w, h), line.at())?);
    let to = across + by;
    [own, along].into_iter().find_map(|along| {
        let (x, y) = swap((along, to));
        let at = match &globe {
            Some(g) => g.inverse(x, y),
            None => cam.try_to_map(x, y, w, h).and_then(|m| map.to_lonlat(m)),
        }?;
        let at = Coord::new((at.x + 180.0).rem_euclid(360.0) - 180.0, at.y);
        if !at.x.is_finite() || !(-90.0..=90.0).contains(&at.y) {
            return None;
        }
        let moved = line.through(at);
        let ends = section_ends(map, cam, (w, h), shape, moved).ok()?;
        // Where the projection has no such place, its inverse can give one that is drawn elsewhere.
        ((swap(ends[0]).1 - to).abs() < 0.5).then_some((moved, ends))
    })
}

/// Where `line` is drawn on a frame `w` × `h` points showing `map` through `cam`, within its
/// `shape`: its ends, the outermost of where it meets the shape's outline (#2863). It is straight
/// on the page whatever the frame's projection or bearing. On a tilted frame or the globe it runs
/// through the place as the frame's grid puts it there (on the ground, without its relief), only as
/// far as there is ground along it, to the horizon or the globe's limb; what it crosses there is no
/// straight section of the ground. `Err` says why it isn't drawn: its place is outside the frame,
/// or one the frame's map can't show (behind the globe or the eye, or outside its projection).
pub fn section_ends(
    map: &Map,
    cam: &rustgis_map::Camera,
    (w, h): (f64, f64),
    shape: &FrameShape,
    line: SectionLine,
) -> Result<SectionEnds, &'static str> {
    const NOT_SHOWN: &str = "its place isn't one the frame's map shows";
    let globe = section_globe(map, cam, (w, h));
    let (px, py) = section_px(map, cam, globe.as_ref(), (w, h), line.at()).ok_or(NOT_SHOWN)?;
    // Along the line and across it: a vertical line is worked out with the axes swapped.
    let vertical = matches!(line, SectionLine::Vertical(_));
    let swap = |(x, y): (f64, f64)| if vertical { (y, x) } else { (x, y) };
    let (along, across) = swap((px, py));
    // Where the line meets the shape's outline, and which way the outline crosses it there: the
    // place is inside as the shape is filled, by its winding number. A circle is as wide as the
    // shorter side ([`shape_path`]).
    let (bw, bh) = if *shape == FrameShape::Circle { (w.min(h), w.min(h)) } else { (w, h) };
    let outline: Vec<(f64, f64)> =
        shape.outline().into_iter().map(|(u, v)| swap(((w - bw) / 2.0 + u * bw, (h - bh) / 2.0 + v * bh))).collect();
    let mut meets: Vec<(f64, i32)> = Vec::new();
    for (a, b) in outline.iter().zip(outline.iter().cycle().skip(1)) {
        if (a.1 <= across) != (b.1 <= across) {
            meets.push((a.0 + (across - a.1) / (b.1 - a.1) * (b.0 - a.0), if a.1 <= across { 1 } else { -1 }));
        }
    }
    let inside = meets.iter().filter(|m| m.0 <= along).map(|m| m.1).sum::<i32>() != 0;
    let (from, to) = meets.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), m| (lo.min(m.0), hi.max(m.0)));
    if !inside || from >= to {
        return Err("its place is outside the frame");
    }
    let ground = |along: f64| {
        let (x, y) = swap((along, across));
        match &globe {
            Some(g) => g.inverse(x, y).is_some(),
            None => cam.pitch.abs() <= 1e-9 || cam.try_to_map(x, y, w, h).is_some(),
        }
    };
    // The ground along the line is in one piece, which holds the place: 24 halvings find its end.
    let reach = |end: f64| {
        let (mut on, mut off) = (along, end);
        if ground(end) {
            return end;
        }
        for _ in 0..24 {
            let mid = (on + off) / 2.0;
            *(if ground(mid) { &mut on } else { &mut off }) = mid;
        }
        on
    };
    Ok([swap((reach(from), across)), swap((reach(to), across))])
}

/// Whether `back`, `at` taken there and back through a projection, is `at`: its longitude a
/// whole turn apart aside, and any at a pole.
fn same_place(back: Coord, at: Coord) -> bool {
    let turns = ((back.x - at.x) / 360.0).round() * 360.0;
    (back.y - at.y).abs() < 1e-6 && (at.y.abs() > 90.0 - 1e-6 || (back.x - turns - at.x).abs() < 1e-6)
}

/// A section line its frame doesn't draw ([`section_ends`]).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SectionGap {
    pub frame: u32,
    pub frame_name: String,
    pub section: String,
    pub why: &'static str,
}

/// The section lines of `layout`'s visible frames they don't draw, as the page is now: those
/// [`section_ends`] gives no ends for, and every one of a frame whose map is missing.
pub fn sections_not_drawn(layout: &Layout, maps: &[Map]) -> Vec<SectionGap> {
    let mut gaps = Vec::new();
    for e in layout.elements.iter().filter(|e| e.visible) {
        let ElementKind::MapFrame { map, center, scale, bearing, pitch, shape, sections, .. } = &e.kind else { continue };
        let shown = map_index(maps, map).and_then(|i| maps.get(i)).map(|m| (m, frame_camera(m, *center, *scale, *bearing, *pitch)));
        let size = (e.w * PT_PER_MM, e.h * PT_PER_MM);
        gaps.extend(sections.iter().filter_map(|s| {
            let why = match &shown {
                Some((m, cam)) => section_ends(m, cam, size, shape, s.line).err()?,
                None => "its frame's map is missing",
            };
            Some(SectionGap { frame: e.id, frame_name: e.name.clone(), section: s.name.clone(), why })
        }));
    }
    gaps
}

/// What an export of `pages` pages says of the section lines not drawn, each with the pages it is
/// missing from, by reason: one notice, the lines counted, the first ten named; `None` for none.
pub fn sections_notice(gaps: &BTreeMap<SectionGap, usize>, pages: usize) -> Option<String> {
    if gaps.is_empty() {
        return None;
    }
    // A line can be missing for one reason on some pages and another on others: one line still.
    let mut lines: BTreeMap<(u32, &str, &str), Vec<String>> = BTreeMap::new();
    for (g, on) in gaps {
        let on = if pages > 1 { format!(" on {on} of {pages} pages") } else { String::new() };
        lines.entry((g.frame, &g.frame_name, &g.section)).or_default().push(format!("{on}: {}", g.why));
    }
    let named = lines.iter().take(10).map(|((frame, name, section), why)| format!("'{section}' of {name} ({frame}){}", why.join(",")));
    let mut named = named.collect::<Vec<_>>().join("; ");
    if lines.len() > 10 {
        named.push_str(&format!("; and {} more", lines.len() - 10));
    }
    Some(format!("{} section line(s) not drawn: {named}", lines.len()))
}

/// The size of a section's name at its ends, in points.
pub(super) const SECTION_NAME_PT: f32 = 8.0;
/// The least share of [`SECTION_NAME_PT`] a line too short for both its names draws them at.
const SECTION_NAME_LEAST: f32 = 0.4;

/// A frame's section lines, each from end to end with its name in a box of its colour at both (A
/// and A'), just inside them and wholly in the frame; on a line too short for both at their size,
/// smaller (down to [`SECTION_NAME_LEAST`] of it), each clear of the other. Gives the names drawn, as [`draw_grid`]
/// gives its labels, each with the edge the credit treats it as: a vertical line's names and a
/// horizontal line's right one as right labels, which it goes left of (above, where it can't), and
/// a horizontal line's left one as a bottom label, which it goes above.
pub(super) fn draw_sections(pm: &mut Canvas, sections: &[(&Section, SectionEnds)], ppp: f64) -> Vec<(Edge, [f32; 4])> {
    let k = ppp as f32;
    let (fw, fh) = (pm.width() as f32, pm.height() as f32);
    // A centre `c` of a box `half` across, inside `0..size` when the box fits there.
    let inside = |c: f32, half: f32, size: f32| if size >= 2.0 * half { c.clamp(half, size - half) } else { c };
    let mut drawn = Vec::new();
    for (s, ends) in sections {
        let [a, b] = ends.map(|(x, y)| (x as f32 * k, y as f32 * k));
        let mut pb = PathBuilder::new();
        pb.move_to(a.0, a.1);
        pb.line_to(b.0, b.1);
        if let Some(p) = pb.finish() {
            let stroke = dashed(Stroke { width: s.width as f32 * k, ..Stroke::default() }, s.dash.as_ref(), ppp, &p);
            pm.stroke_path(&p, &paint(s.color, 1.0), &stroke, Transform::identity(), None);
        }
    }
    // The names over every line.
    for (s, ends) in sections {
        let [a, b] = ends.map(|(x, y)| (x as f32 * k, y as f32 * k));
        let vertical = matches!(s.line, SectionLine::Vertical(_));
        // Half a name's box along the line, its text `px` pixels.
        let half_along =
            |px: f32, name: &str| if vertical { px * 0.8 } else { (text::measure(name, px / 1.33, true) / 2.0 + px * 0.35).max(px * 0.8) };
        let names = [s.name.clone(), format!("{}'", s.name)];
        let length = (b.0 - a.0).max(b.1 - a.1);
        // Each name takes a point at its end and its box's length: smaller where both don't fit.
        let full = SECTION_NAME_PT * 1.33 * k;
        let boxes_long = names.iter().map(|n| 2.0 * half_along(full, n)).sum::<f32>();
        let px = full * ((length - 2.0 * k) / boxes_long).clamp(SECTION_NAME_LEAST, 1.0);
        // Dark on a light colour, or one the map shows through, else white.
        let [r, g, b_, alpha] = s.color.0.map(f32::from);
        let ink = if alpha < 128.0 || 0.299 * r + 0.587 * g + 0.114 * b_ > 150.0 { INK } else { Rgba::rgb(255, 255, 255) };
        let [first, second] = names;
        let boxes = [(a, 1.0, first), (b, -1.0, second)];
        for ((x, y), inward, name) in boxes {
            let hh = px * 0.8;
            let hw = (text::measure(&name, px / 1.33, true) / 2.0 + px * 0.35).max(hh);
            // Inside the end by a point, but never past the line's middle.
            let inset = inward * (k + if vertical { hh } else { hw }).min(length / 2.0);
            // Across the line, the whole box inside the frame too.
            let (cx, cy) = if vertical { (inside(x, hw, fw), y + inset) } else { (x + inset, inside(y, hh, fh)) };
            if let Some(path) = Rect::from_ltrb(cx - hw, cy - hh, cx + hw, cy + hh).and_then(|r| rounded_rect(r, hh)) {
                pm.fill_path(&path, &paint(s.color, 1.0), FillRule::Winding, Transform::identity(), None);
            }
            text::text_at(pm, &name, cx, cy, px, ink, true, text::Align::Center, false);
            // Section names lie anywhere in the frame, not only along its edges as a grid's labels
            // do, so the credit keeps clear of every one, whichever end it is.
            // A vertical line's two names are one above the other: going left clears both, where
            // going above one can meet the other.
            let edge = if vertical || inward < 0.0 { Edge::Right } else { Edge::Bottom };
            drawn.push((edge, [(cx - hw) / k, (cy - hh) / k, (cx + hw) / k, (cy + hh) / k]));
        }
    }
    drawn
}
