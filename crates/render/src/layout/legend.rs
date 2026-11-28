//! The legend: its rows, columns, sizes and the wait for service legend pictures.

use super::*;
use rustgis_map::layout::ClassLabels;

/// Each visible layer's legend for what it draws at a scale denominator (#207), by layer id; a
/// layer that draws nothing there isn't listed. The scale is the one the renderer draws the frame
/// at (the one its layers' scale ranges and display filters see), not the paper scale `{scale}`
/// prints, so the legend follows what the frame actually shows.
pub(super) fn shown_legends(map: &Map, scale: f64, show_no_data: bool, env: &Env) -> Vec<(u32, Vec<LegendEntry>)> {
    map.layers
        .iter()
        .filter(|l| map.draws_at(l, scale) && legend_lists(l))
        .filter_map(|l| layer_shown(l, scale, show_no_data, env).map(|entries| (l.id, entries)))
        .collect()
}

/// Whether a legend can list a layer: a vector layer, by its symbols, or a raster drawn by classes
/// (a colour table, unique values, class breaks), by its classes (#1679). Not a graphics layer,
/// nor a raster on a ramp or in RGB bands, which a colorbar keys or nothing does.
pub fn legend_lists(l: &rustgis_map::Layer) -> bool {
    !l.graphics && l.raster.as_ref().is_none_or(|r| r.style.renderer.draws_classes())
}

/// What a layer a legend lists draws at 1:`scale` ([`rustgis_map::Layer::shown_legend`]); for a
/// classified raster, the classes it holds ([`raster_classes`]). `None` when it draws nothing
/// there.
pub(super) fn layer_shown(l: &rustgis_map::Layer, scale: f64, show_no_data: bool, env: &Env) -> Option<Vec<LegendEntry>> {
    if l.is_raster() {
        return raster_classes(l, false).filter(|classes| l.in_scale_range(scale) && !classes.is_empty());
    }
    let mut entries = l.shown_legend_with(scale, env)?;
    if show_no_data {
        entries.extend(l.renderer.no_data_legend(&l.dataset, Some(&l.drawn_rows_with(scale, env))));
    }
    Some(entries)
}

/// A classified raster's classes as legend rows (#1679), as the Layers pane draws them
/// ([`rustgis_map::RasterLayer::legend_classes`]): a filled swatch with a thin outline, and its
/// label. `None` for a raster on a ramp or in RGB bands, and for a vector layer.
pub(super) fn raster_classes(l: &rustgis_map::Layer, all_classes: bool) -> Option<Vec<LegendEntry>> {
    let classes = l.raster.as_ref()?.legend_classes(all_classes)?;
    let symbol = |fill| rustgis_map::Symbol { fill, stroke: RASTER_SWATCH_EDGE, stroke_width: 0.5, size: 0.0, marker: Default::default() };
    Some(classes.into_iter().map(|(fill, label)| LegendEntry::plain(label, symbol(fill))).collect())
}

/// Names only the source whose read stopped this page, leaving unrelated failures generic.
pub(super) fn failed_source_error(map: &Map, failed_source: Option<u64>) -> Option<String> {
    let id = failed_source?;
    let layer = map.layers.iter().find(|layer| layer.instance_id() == id)?;
    layer.source.as_ref()?.read_error().map(|why| format!("{}: {why}", layer.name))
}

/// Complete-source legends retain one minimal value per used class, never its geometry. A layer
/// named in `exclude` isn't read from its source. A failed traversal returns its live layer
/// identity for the page's source-error report.
pub(super) fn complete_shown_legends(
    map: &mut Map,
    scale: f64,
    show_no_data: bool,
    exclude: &[String],
    failed_source: &mut Option<u64>,
    env: &Env,
) -> Option<Vec<(u32, Vec<LegendEntry>)>> {
    *failed_source = None;
    if !map.layers.iter().any(|l| l.source.as_ref().and_then(|s| s.chunk_source()).is_some()) {
        return Some(shown_legends(map, scale, show_no_data, env));
    }
    let style_env = env.at_scale(Some(scale));
    // A layer left out is not read: its rows aren't listed, and its failure doesn't stop the rest.
    let excluded = |name: &str| exclude.iter().any(|n| n.eq_ignore_ascii_case(name));
    let wanted: Vec<usize> = map
        .layers
        .iter()
        .enumerate()
        .filter(|(_, l)| map.draws_at(l, scale) && legend_lists(l) && !excluded(&l.name))
        .map(|(i, _)| i)
        .collect();
    let mut out = Vec::new();
    for index in wanted {
        let layer = map.layers.get_mut(index)?;
        let Some(source) = layer.source.as_ref().and_then(|s| s.chunk_source()) else {
            if let Some(entries) = layer_shown(layer, scale, show_no_data, env) {
                out.push((layer.id, entries));
            }
            continue;
        };
        let unique = match &layer.renderer {
            rustgis_map::Renderer::Unique { field, classes, .. } => Some((field, classes)),
            _ => None,
        };
        // By expression: a feature of each class seen is kept whole, for the legend to class again.
        let by_expression = layer.renderer.expression().is_some();
        let fi = unique.filter(|_| !by_expression).and_then(|(field, _)| layer.dataset.field_index(field));
        let fields = if by_expression {
            layer.dataset.fields.clone()
        } else {
            unique
                .filter(|_| fi.is_some())
                .map_or_else(Vec::new, |(field, _)| vec![rustgis_data::Field::new(field, rustgis_data::FieldType::Text)])
        };
        let mut sample = rustgis_data::Dataset::new("legend", layer.geometry_type(), fields);
        let mut used = std::collections::HashSet::new();
        let mut any = false;
        let mut no_data = None;
        let mut chunk = layer.clone();
        chunk.source = None;
        // Its chunks come in the source's own system.
        chunk.dataset.held_in = source.coord_crs();
        chunk.clear_cache();
        let result = crate::visit_complete(source.as_ref(), &mut |features| {
            chunk.dataset.features = features.into();
            let rows = chunk.drawn_rows_with(scale, env);
            if show_no_data && no_data.is_none() {
                no_data = chunk.renderer.no_data_legend(&chunk.dataset, Some(&rows));
            }
            let classer = by_expression.then(|| chunk.renderer.classer_with(&chunk.dataset, style_env.clone()));
            for row in rows.iter().enumerate().filter_map(|(row, keep)| keep.then_some(row)) {
                any = true;
                if let Some(c) = &classer {
                    let key = chunk.renderer.unique_class(&chunk.dataset, row, c).unwrap_or(usize::MAX);
                    // Kept with its id ($id), without its geometry.
                    if used.insert(key)
                        && let Some(f) = chunk.dataset.features.get(row)
                    {
                        let mut f = f.clone();
                        f.geometry = None;
                        sample.features.push(f);
                    }
                } else if let Some((_, classes)) = unique {
                    let value = fi.map(|fi| chunk.dataset.value(row, fi).to_string());
                    let key = value.as_ref().and_then(|v| classes.iter().position(|(k, _)| k == v)).unwrap_or(classes.len());
                    if used.insert(key) {
                        sample.push(None, value.map_or_else(Vec::new, |v| vec![rustgis_data::Value::Text(v)]));
                    }
                }
            }
            chunk.dataset.features = Vec::new().into();
            Ok(())
        });
        if let Err(e) = result {
            if let Some(source) = &mut layer.source {
                source.error = Some(e.to_string());
            }
            *failed_source = Some(layer.instance_id());
            return None;
        }
        if let Some(source) = &mut layer.source {
            source.clear_error();
        }
        if any {
            // (A sample of the classes seen: classed again at this scale, by expression.)
            let mut entries = layer.with_size_legend(layer.renderer.legend_with(&sample, &[], style_env.clone()));
            entries.extend(no_data);
            out.push((layer.id, entries));
        }
    }
    Some(out)
}

/// What a legend leaves out: `<all other values>`, layers' names over their classes and pictures
/// (unless `names`), and layers by name; the labels it shows for layers instead of their names; the
/// scale denominator whose legend pictures a WMS or WMTS layer shows; its text size in points, and
/// how many times its default size its rows are drawn at ([`legend_size`]): that of `text_size`,
/// until [`fit_legend`] picks another.
#[derive(Clone, Copy)]
pub(super) struct LegendOptions<'a> {
    pub(super) hide_other: bool,
    pub(super) show_no_data: bool,
    pub(super) names: bool,
    pub(super) exclude: &'a [String],
    pub(super) labels: &'a BTreeMap<String, String>,
    pub(super) class_labels: &'a ClassLabels,
    pub(super) scale: f64,
    pub(super) size: f32,
    pub(super) text_size: f64,
}

/// How many times the default a legend of `text_size` points draws its text, rows, swatches and
/// gaps (#2581): 1 at [`rustgis_map::layout::LEGEND_TEXT_PT`], as legends have always been drawn.
/// A point symbol keeps the size the map draws it at, so a legend of sizes still matches the map.
pub(super) fn legend_size(text_size: f64) -> f32 {
    layout_text_scale(text_size, rustgis_map::layout::LEGEND_TEXT_PT) as f32
}

impl<'a> LegendOptions<'a> {
    /// What the legend shows for the layer named `name`: its label (by name, any case), or the
    /// name when it has none (#1654).
    pub(super) fn shown(&self, name: &'a str) -> &'a str {
        let label = self.labels.get(name).or_else(|| self.labels.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, l)| l));
        label.map(String::as_str).filter(|l| !l.trim().is_empty()).unwrap_or(name)
    }
}

/// A row of a legend: a layer drawn in one symbol (its swatch and name), a layer's name over its
/// classes, or one of them, or a WMS or WMTS layer's legend picture (under its name). The name is
/// the one the legend shows: the layer's label, if the legend gives it one.
pub(super) enum LegendRow<'a> {
    Plain(&'a rustgis_map::Layer, &'a str),
    Head(&'a str),
    Class(&'a rustgis_map::Layer, LegendEntry),
    Picture(std::sync::Arc<rustgis_tiles::cache::Payload>),
}

impl LegendRow<'_> {
    /// The side of the square its point symbol takes at 1:`scale`, in points, as far as its marks
    /// reach ([`point_marks::reach`]); 0 for any other row. No larger than a legend picture.
    pub(super) fn symbol_side(&self, scale: f64) -> f32 {
        #[cfg(test)]
        tests::SYMBOL_SIDES.with(|n| n.set(n.get() + 1));
        let (LegendRow::Plain(l, _) | LegendRow::Class(l, _)) = self else { return 0.0 };
        if l.geometry_type() != GeometryType::Point {
            return 0.0;
        }
        let (s, strokes) = match self {
            LegendRow::Class(_, en) => (en.symbol, row_strokes(l, en)),
            _ => (l.renderer.symbol_for(&l.dataset, 0), l.line_strokes.as_slice()),
        };
        (2.0 * point_marks::reach(&s, strokes) * l.size_factor(scale)).min(MAX_LEGEND_SIDE as f32)
    }

    /// Its height at 1:`scale` in a legend `size` times the default ([`legend_size`]), in points:
    /// a picture's a pixel a point at the default size, as the Layers pane shows it; a row grows to
    /// hold its point symbol.
    #[cfg(test)]
    pub(super) fn height(&self, scale: f64, size: f32) -> f32 {
        match self {
            LegendRow::Picture(_) => self.height_beside(0.0, size),
            _ => self.height_beside(self.symbol_side(scale), size),
        }
    }

    /// [`LegendRow::height`], its point symbol `side` points across ([`LegendRow::symbol_side`]).
    fn height_beside(&self, side: f32, size: f32) -> f32 {
        match self {
            LegendRow::Picture(p) => picture_size(p).1 * size,
            _ => (LEGEND_ROW * size).max(side),
        }
    }

    /// How wide its name or label is in a legend `size` times the default, in points; 0 for a
    /// picture, which has none.
    pub(super) fn text_width(&self, size: f32) -> f32 {
        let (shown, pt, bold) = match self {
            LegendRow::Plain(_, name) => (*name, LEGEND_NAME_PT * size, false),
            LegendRow::Head(name) => (*name, LEGEND_NAME_PT * size, true),
            LegendRow::Class(_, en) => (en.label.as_str(), LEGEND_LABEL_PT * size, false),
            LegendRow::Picture(_) => return 0.0,
        };
        #[cfg(test)]
        tests::TEXT_MEASURES.with(|n| n.set(n.get() + 1));
        text::measure(shown, pt, bold)
    }

    /// Its width beside a column of swatches `column` points wide ([`swatch_column`]) in a legend
    /// `size` times the default, its text `text` points wide ([`LegendRow::text_width`]), in points;
    /// a picture's, `text` itself ([`RowSizes::of`]).
    pub(super) fn width(&self, column: f32, size: f32, text: f32) -> f32 {
        match self {
            LegendRow::Plain(..) => column + LEGEND_INDENT * size + text,
            LegendRow::Head(_) => text,
            LegendRow::Class(..) => column + LEGEND_LABEL_INDENT * size + text,
            LegendRow::Picture(_) => text,
        }
    }
}

/// The side of each of a legend's `rows`' point symbols at 1:`scale` ([`LegendRow::symbol_side`]),
/// which no text size or column count changes, so it is measured once a legend.
pub(super) fn symbol_sides(rows: &[LegendRow], scale: f64) -> Vec<f32> {
    rows.iter().map(|row| row.symbol_side(scale)).collect()
}

/// A legend's rows measured at a size `size` times the default ([`legend_size`]), in points, in
/// their order: each row's height and the width of its text (of a legend picture, after its
/// indent), made once a size.
pub(super) struct RowSizes {
    size: f32,
    heights: Vec<f32>,
    texts: Vec<f32>,
}

impl RowSizes {
    /// `rows` beside their point symbols' `sides` ([`symbol_sides`]), measured at `size` times the
    /// default.
    pub(super) fn of(rows: &[LegendRow], sides: &[f32], size: f32) -> Self {
        let heights = rows.iter().zip(sides).map(|(row, &side)| row.height_beside(side, size)).collect();
        let texts = rows
            .iter()
            .map(|row| match row {
                LegendRow::Picture(p) => (LEGEND_INDENT + picture_size(p).0) * size,
                _ => row.text_width(size),
            })
            .collect();
        RowSizes { size, heights, texts }
    }

    /// These measures as a box `room` points wide draws the rows: a legend picture wider than it,
    /// after its indent, narrowed to it and shortened with it, as [`legend_picture`] draws it
    /// ([`picture_scale`]); one that can't be narrowed to anything keeps its size.
    fn narrowed(mut self, rows: &[LegendRow], room: f32) -> Self {
        for ((row, height), width) in rows.iter().zip(&mut self.heights).zip(&mut self.texts) {
            if let LegendRow::Picture(p) = row
                && *width > room
            {
                let (w, h) = picture_size(p);
                let k = ((room - LEGEND_INDENT * self.size).max(0.0) / w.max(1.0)).min(self.size);
                if k > 0.0 {
                    (*width, *height) = (LEGEND_INDENT * self.size + w * k, h * k);
                }
            }
        }
        self
    }
}

/// A legend's rows with their measures at one size: their point symbols' sides
/// ([`symbol_sides`]), their heights and their texts' widths ([`RowSizes`]). [`flow`],
/// [`balanced`], [`widest`] and [`measure`] take a row's size only from these, so the drawing and
/// the fit lay rows out by the one rule, and every column count tried at a size reuses its
/// measures. Each slice is as long as the rows.
#[derive(Clone, Copy)]
pub(super) struct Measured<'m, 'a> {
    rows: &'m [LegendRow<'a>],
    sides: &'m [f32],
    heights: &'m [f32],
    texts: &'m [f32],
    /// How many times the default the rows are measured at ([`legend_size`]).
    size: f32,
}

impl<'m, 'a> Measured<'m, 'a> {
    /// `rows` beside their point symbols' `sides`, with their `sizes` at one size.
    pub(super) fn new(rows: &'m [LegendRow<'a>], sides: &'m [f32], sizes: &'m RowSizes) -> Self {
        Measured { rows, sides, heights: &sizes.heights, texts: &sizes.texts, size: sizes.size }
    }

    /// Its rows in `range`, with their measures.
    fn part(self, range: std::ops::Range<usize>) -> Self {
        let cut = |v: &'m [f32]| v.get(range.clone()).unwrap_or_default();
        let rows = self.rows.get(range.clone()).unwrap_or_default();
        Measured { rows, sides: cut(self.sides), heights: cut(self.heights), texts: cut(self.texts), size: self.size }
    }

    /// The rows of each column, from where they start, with their measures.
    fn parts(self, starts: &[usize]) -> impl Iterator<Item = Self> {
        column_ranges(starts, self.rows.len()).map(move |range| self.part(range))
    }
}

/// The rows `m` laid into at most `columns` columns `limit` points high: the row each column
/// starts at, and whether they all fit. A layer's name stays with the row under it; a row taller
/// than `limit` has a column to itself, and the last column takes what is left.
pub(super) fn flow(m: Measured, columns: usize, limit: f32) -> (Vec<usize>, bool) {
    // A thousandth of a row's slack, as the rows have when drawn.
    let limit = limit + LEGEND_ROW * m.size * 1e-3;
    let rows = m.rows;
    let (mut starts, mut used, mut fits, mut i) = (vec![0], 0.0f32, true, 0);
    while i < rows.len() {
        let end = if matches!(rows[i], LegendRow::Head(_)) { (i + 2).min(rows.len()) } else { i + 1 };
        let part: f32 = m.heights.get(i..end).unwrap_or_default().iter().copied().sum();
        if used > 0.0 && used + part > limit && starts.len() < columns {
            starts.push(i);
            used = 0.0;
        }
        used += part;
        fits &= used <= limit;
        i = end;
    }
    (starts, fits)
}

/// The rows `m` laid into at most `columns` columns as evenly as they go: where each starts
/// ([`flow`]), and the height of the tallest, in points.
pub(super) fn balanced(m: Measured, columns: usize) -> (Vec<usize>, f32) {
    let total: f32 = m.heights.iter().copied().sum();
    let (mut low, mut high) = (total / columns as f32, total);
    for _ in 0..24 {
        let middle = (low + high) / 2.0;
        if flow(m, columns, middle).1 {
            high = middle;
        } else {
            low = middle;
        }
    }
    let starts = flow(m, columns, high).0;
    let tallest = m.parts(&starts).map(|part| part.heights.iter().copied().sum()).fold(0.0f32, f32::max);
    (starts, tallest)
}

/// The rows of each column, from where they start.
#[cfg(test)]
pub(super) fn column_parts<'r, 'a>(rows: &'r [LegendRow<'a>], starts: &'r [usize]) -> impl Iterator<Item = &'r [LegendRow<'a>]> {
    column_ranges(starts, rows.len()).map(|range| rows.get(range).unwrap_or_default())
}

/// Where each column's rows run, from where they start, the last to the `len`th row.
fn column_ranges(starts: &[usize], len: usize) -> impl Iterator<Item = std::ops::Range<usize>> {
    starts.iter().enumerate().map(move |(k, &start)| start..starts.get(k + 1).copied().unwrap_or(len))
}

/// The widest of a column's rows `m`, in points.
pub(super) fn widest(m: Measured) -> f32 {
    let column = swatch_column(m.sides.iter().copied(), m.size);
    m.rows.iter().zip(m.texts).map(|(row, &text)| row.width(column, m.size, text)).fold(0.0, f32::max)
}

/// The stacked strokes a legend's class row draws: its class's own, else its layer's, as
/// [`rustgis_map::Layer::strokes`] finds them for a feature.
pub(super) fn row_strokes<'a>(l: &'a rustgis_map::Layer, en: &'a LegendEntry) -> &'a [rustgis_map::LineStroke] {
    if en.strokes.is_empty() { &l.line_strokes } else { &en.strokes }
}

/// The stacked fills a legend's class row draws: its class's own, else its layer's, as
/// [`rustgis_map::Layer::fills`] finds them for a feature.
pub(super) fn row_fills<'a>(l: &'a rustgis_map::Layer, en: &'a LegendEntry) -> &'a [rustgis_map::fills::FillLayer] {
    if en.fills.is_empty() { &l.fill_layers } else { &en.fills }
}

/// The width of a legend's column of swatches beside point symbols `sides` points across
/// ([`LegendRow::symbol_side`]) in a legend `size` times the default, in points: it grows to hold
/// the widest point symbol, so the labels line up.
pub(super) fn swatch_column(sides: impl IntoIterator<Item = f32>, size: f32) -> f32 {
    sides.into_iter().fold(LEGEND_SWATCH * size, f32::max)
}

/// A legend picture's size in points (a pixel a point).
pub(super) fn picture_size(p: &rustgis_tiles::cache::Payload) -> (f32, f32) {
    match p {
        rustgis_tiles::cache::Payload::Image(pm) => (pm.width() as f32, pm.height() as f32),
        _ => (0.0, 0.0),
    }
}

/// Whether a legend lists a WMS or WMTS layer: drawn, and with a legend.
pub fn tile_listed(t: &rustgis_map::TileLayer) -> bool {
    t.draws() && t.opacity > 0.0 && !t.legends.is_empty()
}

/// A WMS or WMTS layer's legend picture for the map at 1:`scale`, as the Layers pane shows it:
/// the first of its legends there that is a picture. `None` while the one to show is still on
/// its way (`pending` then set), or when none can be shown. The addresses come from a service or
/// a project, so they are checked as the layer's own are.
pub(super) fn tile_legend(
    t: &rustgis_map::TileLayer,
    scale: f64,
    pending: &mut bool,
) -> Option<std::sync::Arc<rustgis_tiles::cache::Payload>> {
    use rustgis_tiles::cache::{self, Payload, State, Want};
    for l in rustgis_data::ogc::legends_at(&t.legends, scale) {
        if rustgis_tiles::address::check(&l.url, Some(&t.url)).is_err() {
            continue;
        }
        match cache::get(&l.url, Want::Image) {
            // No larger than the Layers pane shows one.
            State::Ready(p) if matches!(&*p, Payload::Image(pm) if pm.width() <= MAX_LEGEND_SIDE && pm.height() <= MAX_LEGEND_SIDE) => {
                return Some(p);
            }
            State::Pending => {
                *pending = true;
                return None;
            }
            // The next, if there is one.
            _ => {}
        }
    }
    None
}

/// Whether a legend picture `map`'s WMS and WMTS layers show at 1:`scale` is still on its way
/// (asking for those not yet asked for); those `exclude` names aren't listed, so aren't waited for.
pub fn tile_legends_pending(map: &Map, scale: f64, exclude: &[String]) -> bool {
    let mut pending = false;
    for t in map.tiles.iter().filter(|t| tile_listed(t) && !exclude.iter().any(|n| n.eq_ignore_ascii_case(&t.name))) {
        let _ = tile_legend(t, scale, &mut pending);
    }
    pending
}

/// Whether an element lies at least partly on `layout`'s page. The page's drawing and the redraw
/// gate ([`legends_pending`]) share this one test, in millimetres, so no drawing resolution can make
/// them disagree about a legend at the page's edge.
pub(super) fn on_page(layout: &Layout, e: &rustgis_map::layout::Element) -> bool {
    e.x + e.w > 0.0 && e.y + e.h > 0.0 && e.x < layout.width && e.y < layout.height
}

/// Whether a legend on the page `layout` lists a WMS or WMTS layer whose legend picture is still
/// on its way, at its frame's scale. The interactive page is drawn again once it lands. A legend
/// wholly off the page shows none of its pictures, so it is not waited for.
pub fn legends_pending(layout: &Layout, maps: &[Map]) -> bool {
    layout.elements.iter().filter(|e| e.visible && on_page(layout, e)).any(|e| {
        let ElementKind::Legend { frame, exclude, .. } = &e.kind else { return false };
        let Some(ElementKind::MapFrame { map, center, scale, bearing, pitch, .. }) = layout.element(*frame).map(|f| &f.kind) else {
            return false;
        };
        let Some(m) = maps.iter().find(|m| m.name.eq_ignore_ascii_case(map)) else { return false };
        let cam = frame_camera(m, *center, *scale, *bearing, *pitch);
        tile_legends_pending(m, m.ground_res_at(&cam) / rustgis_geom::PIXEL_M, exclude)
    })
}

/// Refuses an export stopped during a legend wait, or that drew a legend whose picture was still
/// on its way: it stays refused if the picture lands afterwards, as the file would still lack it.
/// The browser cannot wait; it asks the caller to export again once the pictures land.
pub(crate) fn finish_legends() -> Result<(), String> {
    rustgis_data::progress::check().map_err(|e| e.to_string())?;
    if crate::cancel::requested() {
        return Err(rustgis_data::DataError::Cancelled.to_string());
    }
    if LEGEND_MISSING.get() {
        return Err(
            "legend pictures were still on their way when the export stopped waiting for them; export again once they arrive".into()
        );
    }
    Ok(())
}

/// How long an export waits, natively, for all the legend pictures it shows.
pub(super) const LEGEND_WAIT: web_time::Duration = web_time::Duration::from_secs(30);

thread_local! {
    /// While an export draws on this thread: `Some`, holding the deadline for its legend pictures
    /// once its first legend has waited. Every page, element and fallback picture of that export
    /// shares it.
    pub(super) static LEGEND_BUDGET: std::cell::Cell<Option<Option<web_time::Instant>>> = const { std::cell::Cell::new(None) };
    /// Whether a legend of the export drawing on this thread was drawn without a picture.
    pub(super) static LEGEND_MISSING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// A shorter [`LEGEND_WAIT`] for a test to reach.
    #[cfg(test)]
    pub(super) static TEST_LEGEND_WAIT: std::cell::Cell<Option<web_time::Duration>> = const { std::cell::Cell::new(None) };
}

/// Ends the budget [`legend_budget`] opened when dropped, even by a panic.
pub(crate) struct LegendBudget(bool);

impl Drop for LegendBudget {
    fn drop(&mut self) {
        if self.0 {
            LEGEND_BUDGET.set(None);
            LEGEND_MISSING.set(false);
        }
    }
}

/// Opens one legend-wait budget for the export drawing on this thread, unless one is open (an
/// export of several pages or elements, each drawn through its own call, shares it).
pub(crate) fn legend_budget() -> LegendBudget {
    let opened = LEGEND_BUDGET.get().is_none();
    if opened {
        LEGEND_BUDGET.set(Some(None));
        LEGEND_MISSING.set(false);
    }
    LegendBudget(opened)
}

/// A legend under a fit other than manual that the page drew still wider or taller than the room
/// its title leaves in its box ([`unfitted_legends`]): the columns and verdict of the very fit
/// ([`fit_legend`]) its rows were drawn by.
#[derive(Clone, Debug, PartialEq)]
pub struct UnfittedLegend {
    /// The legend element's id.
    pub id: u32,
    /// The legend element's name, as the layout holds it.
    pub name: String,
    pub fit: LegendFit,
    /// The columns its rows were drawn in.
    pub columns: usize,
    pub too_wide: bool,
    pub too_tall: bool,
}

thread_local! {
    /// While [`unfitted_legends`] runs on this thread: the fitted legends drawn that still don't
    /// fit, each element once for each way it didn't.
    static UNFITTED: std::cell::RefCell<Option<Vec<UnfittedLegend>>> = const { std::cell::RefCell::new(None) };
}

/// Ends the list [`unfitted_legends`] opened when dropped, even by a panic.
struct UnfittedList(bool);

impl Drop for UnfittedList {
    fn drop(&mut self) {
        if self.0 {
            UNFITTED.set(None);
        }
    }
}

/// `f`, and the legends under a fit other than manual that the pages it drew on this thread drew
/// still wider or taller than their boxes, each element once for each way it was (too wide, too
/// tall or both: a map series draws a legend a page, and it can fail differently on two), the
/// first time it was, with the columns of that page: what an
/// export says of them, as setting the fit said it, since a fit can stop succeeding after it is
/// set (a layer added to its map, its frame's scale changed, a legend picture arrived). A manual
/// legend is never listed. A call inside another lists nothing and leaves its legends to the
/// outer one.
pub fn unfitted_legends<T>(f: impl FnOnce() -> T) -> (T, Vec<UnfittedLegend>) {
    let opened = UNFITTED.with_borrow_mut(|u| {
        let none = u.is_none();
        if none {
            *u = Some(Vec::new());
        }
        none
    });
    let _list = UnfittedList(opened);
    let out = f();
    let listed = if opened { UNFITTED.with_borrow_mut(Option::take).unwrap_or_default() } else { Vec::new() };
    (out, listed)
}

/// Lists the legend `id` named `name` under `fit`, drawn as `fitted`, while [`unfitted_legends`]
/// runs.
pub(super) fn unfitted((id, name): (u32, &str), fit: LegendFit, fitted: &Fitted) {
    UNFITTED.with_borrow_mut(|u| {
        if let Some(listed) = u
            && !listed.iter().any(|l| (l.id, l.too_wide, l.too_tall) == (id, fitted.too_wide, fitted.too_tall))
        {
            listed.push(UnfittedLegend {
                id,
                name: name.to_string(),
                fit: fit.kept(),
                columns: fitted.columns,
                too_wide: fitted.too_wide,
                too_tall: fitted.too_tall,
            });
        }
    });
}

/// When the open budget's legend wait ends, started by the first legend that waits.
pub(super) fn legend_deadline() -> web_time::Instant {
    #[cfg(test)]
    let wait = TEST_LEGEND_WAIT.get().unwrap_or(LEGEND_WAIT);
    #[cfg(not(test))]
    let wait = LEGEND_WAIT;
    let fresh = web_time::Instant::now() + wait;
    match LEGEND_BUDGET.get() {
        Some(Some(deadline)) => deadline,
        Some(None) => {
            LEGEND_BUDGET.set(Some(Some(fresh)));
            fresh
        }
        None => fresh,
    }
}

/// Waits (natively, until `deadline`) for the legend pictures a legend of `map` at 1:`scale`
/// shows, as a frame waits for its tiles: a page needs them all. Only for those: not for the
/// cache's other fetches, nor for a layer the legend leaves out (`exclude`). `None` when the
/// run was cancelled meanwhile.
pub(super) fn wait_for_tile_legends(map: &Map, scale: f64, exclude: &[String], deadline: web_time::Instant) -> Option<()> {
    // In the browser, which can't wait, the page draws what has arrived.
    while cfg!(not(target_arch = "wasm32")) && web_time::Instant::now() < deadline && tile_legends_pending(map, scale, exclude) {
        if crate::cancel::requested() || rustgis_data::progress::check().is_err() {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
    Some(())
}

/// The widest and tallest legend picture shown, in pixels, as in the Layers pane, and the largest
/// side of a legend's point symbol, in points.
pub(super) const MAX_LEGEND_SIDE: u32 = 2048;

/// The outline of a raster class's swatch in a legend, as the Layers pane draws one around it.
pub(super) const RASTER_SWATCH_EDGE: Rgba = Rgba::rgb(0x80, 0x80, 0x80);
/// A legend row's height, in points.
pub(super) const LEGEND_ROW: f32 = 15.0;
/// A legend's title, layers' names and classes' labels, in points, at its default text size; the
/// room its title takes, in rows; and how far a class's swatch and a name's or label's text sit
/// in, in points. Measuring and drawing a legend both take them, times its size ([`legend_size`]).
pub(super) const LEGEND_TITLE_PT: f32 = 12.0;
pub(super) const LEGEND_NAME_PT: f32 = 9.0;
pub(super) const LEGEND_LABEL_PT: f32 = 8.5;
pub(super) const LEGEND_TITLE_ROWS: f32 = 1.3;
pub(super) const LEGEND_INDENT: f32 = 6.0;
pub(super) const LEGEND_LABEL_INDENT: f32 = 12.0;
/// A legend swatch's width, in points.
pub(super) const LEGEND_SWATCH: f32 = 16.0;
/// The room between a legend's columns, in points.
pub(super) const LEGEND_COLUMN_GAP: f32 = 10.0;

/// The step a fit that shrinks a legend's text takes it down by, in points.
pub(super) const LEGEND_FIT_STEP_PT: f64 = 0.25;

/// The columns and text size [`fit_legend`] draws a legend's rows at, and whether its rows are
/// still wider or taller than its room there.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Fitted {
    /// 1 to [`MAX_LEGEND_COLUMNS`].
    pub(super) columns: usize,
    /// How many times the default its rows are drawn: [`legend_size`] of `text_size`, the very
    /// computation a legend's own size is made by, so a manual legend keeps its size bit for bit.
    pub(super) size: f32,
    /// In points, within [`LAYOUT_TEXT_SIZE`].
    pub(super) text_size: f64,
    /// The row each column starts at as the page draws them ([`drawn_starts`]), which its width
    /// is measured in: the drawing lays its rows out in these.
    starts: Vec<usize>,
    pub(super) too_wide: bool,
    pub(super) too_tall: bool,
}

impl Fitted {
    /// A fit's verdict alone, for a test of what is listed.
    #[cfg(test)]
    pub(super) fn verdict(columns: usize, too_wide: bool, too_tall: bool) -> Fitted {
        Fitted { columns, size: 1.0, text_size: 9.0, starts: vec![0], too_wide, too_tall }
    }

    /// Neither wider nor taller than its room.
    pub(super) fn fits(&self) -> bool {
        !self.too_wide && !self.too_tall
    }
}

/// Whether a legend's rows `measured` points across or down, `size` times the default, take more
/// than `room` points by more than a thousandth of a row: a measure that is no number never fits.
#[expect(clippy::neg_cmp_op_on_partial_ord, reason = "a measure that is no number compares as neither, and is to count as not fitting")]
pub(super) fn beyond(measured: f32, room: f32, size: f32) -> bool {
    !(measured <= room + LEGEND_ROW * size * 1e-3)
}

/// The one rule for the columns and text size a legend's `rows` are drawn at in a room of
/// `room_w` × `room_h` points (what its box leaves under its title), from its `columns` and its
/// `text_size` in points, at 1:`scale`. A candidate is measured as the page draws it: its
/// tallest column with its rows as even as they go in that many columns ([`balanced`]), and its
/// width in the columns its rows are then laid in within the room ([`drawn_starts`]), side by side
/// with their gaps ([`across`]); it fits when it is neither wider nor taller than the room by more
/// than a thousandth of a row. Too tall, its columns are filled in turn to the room's height, so
/// its width is that of those columns, not of the even ones. A legend picture wider than the room
/// is measured narrowed to it, as its column draws it ([`RowSizes::narrowed`]), where a manual
/// legend measures it a pixel a point, as it always has.
///
/// - Manual (and a value that is no fit): the columns and size as set.
/// - adjustSize: in the columns set, the size [`largest_fitting`] finds among `text_size` and the
///   sizes a quarter point apart below it down to [`LAYOUT_TEXT_SIZE`]'s least (the largest that
///   fits, as a rule); else that least.
/// - adjustColumns: at `text_size`, the fewest columns from those set up to [`MAX_LEGEND_COLUMNS`]
///   that fit; else the most whose width still fits across (those set when none does), so a fit
///   never adds a column the box hasn't the width for.
/// - adjustColumnsAndSize: the size found the same way at which some of those columns fit, in the
///   fewest that do; else the least size, in the most columns that fit across at it.
///
/// A fitted legend is drawn as a manual legend of the columns and size returned, and `too_wide`
/// and `too_tall` say what still doesn't fit there. A room that is no number or below 0 is no room;
/// no rows always fit. At most 11 sizes are tried ([`largest_fitting`]), in at most
/// [`MAX_LEGEND_COLUMNS`] columns each: 66 layouts, whatever is asked. The rows' point symbols are
/// measured once, and their heights and texts once a size tried ([`Trials`]).
pub(super) fn fit_legend(
    rows: &[LegendRow],
    (columns, fit): (usize, LegendFit),
    (room_w, room_h): (f32, f32),
    (scale, text_size): (f64, f64),
) -> Fitted {
    fit_measured(rows, &symbol_sides(rows, scale), (columns, fit), (room_w, room_h), text_size).0
}

/// [`fit_legend`] of `rows` beside their point symbols' `sides` ([`symbol_sides`]), and the rows'
/// measures at the size it picks, which the drawing lays them out by.
fn fit_measured(
    rows: &[LegendRow],
    sides: &[f32],
    (columns, fit): (usize, LegendFit),
    (room_w, room_h): (f32, f32),
    text_size: f64,
) -> (Fitted, RowSizes) {
    let safe = |v: f32| if v.is_nan() { 0.0 } else { v.max(0.0) };
    let n = legend_columns_in_range(columns);
    let text_size = layout_text_in_range(text_size, LEGEND_TEXT_PT);
    let mut trials = Trials { rows, sides, room: (safe(room_w), safe(room_h)), sizes: Vec::new() };
    let fitted = match fit.kept() {
        LegendFit::Manual | LegendFit::Invalid => trials.at(n, text_size),
        LegendFit::AdjustSize => largest_fitting(text_size, |pt| {
            let f = trials.at(n, pt);
            let fits = f.fits();
            (f, fits)
        }),
        LegendFit::AdjustColumns => trials.columns_at(n, text_size).0,
        LegendFit::AdjustColumnsAndSize => largest_fitting(text_size, |pt| trials.columns_at(n, pt)),
    };
    let sizes = trials.take(fitted.size);
    (fitted, sizes)
}

/// What [`fit_measured`] tries a legend's rows at: the rows, their point symbols' sides, the room
/// they have (a number, 0 or more), and their measures at each size tried so far, which every
/// column count tried at that size shares.
struct Trials<'m, 'a> {
    rows: &'m [LegendRow<'a>],
    sides: &'m [f32],
    room: (f32, f32),
    sizes: Vec<RowSizes>,
}

impl Trials<'_, '_> {
    /// The rows in `columns` columns at `pt` points, measured as the page draws them: as tall as
    /// their even columns ([`balanced`]), as wide as the columns they are laid in within the room
    /// ([`drawn_starts`]).
    fn at(&mut self, columns: usize, pt: f64) -> Fitted {
        let size = legend_size(pt);
        let sizes = self.take(size);
        let m = Measured::new(self.rows, self.sides, &sizes);
        let tallest = balanced(m, columns).1;
        let starts = drawn_starts(m, columns, tallest, self.room.1);
        let width = across(m, &starts);
        self.sizes.push(sizes);
        let listed = !self.rows.is_empty();
        let (too_wide, too_tall) = (listed && beyond(width, self.room.0, size), listed && beyond(tallest, self.room.1, size));
        Fitted { columns, size, text_size: pt, starts, too_wide, too_tall }
    }

    /// The rows' measures at `size` times the default as the room draws them
    /// ([`RowSizes::narrowed`]): those made before, else made now.
    fn take(&mut self, size: f32) -> RowSizes {
        match self.sizes.iter().position(|s| s.size.to_bits() == size.to_bits()) {
            Some(k) => self.sizes.swap_remove(k),
            None => RowSizes::of(self.rows, self.sides, size).narrowed(self.rows, self.room.0),
        }
    }

    /// From `n` columns up at `pt` points: the fewest that fit, else the most that fit across,
    /// else `n`; and whether it fits.
    fn columns_at(&mut self, n: usize, pt: f64) -> (Fitted, bool) {
        let first = self.at(n, pt);
        if first.fits() {
            return (first, true);
        }
        // The most beyond `n` that fit across; `n` itself when none does, whether or not it does.
        let mut across = None;
        for c in n + 1..=MAX_LEGEND_COLUMNS {
            let f = self.at(c, pt);
            if f.fits() {
                return (f, true);
            }
            if !f.too_wide {
                across = Some(f);
            }
        }
        (across.unwrap_or(first), false)
    }
}

/// Of `text_size` and the sizes a step ([`LEGEND_FIT_STEP_PT`]) apart below it down to
/// [`LAYOUT_TEXT_SIZE`]'s least (that least the last, where the steps skip it), one that `try_at`
/// says fits, found by halving the list, else the least. That is the largest that fits when a
/// smaller text fits wherever a larger one does, which holds but for a row moved to another column
/// by a rounding; then it can be a smaller size that fits, never one that doesn't. The list holds
/// at most 385 sizes, `text_size` being in range, so `try_at` is asked at most 11 times, where
/// trying each size in turn would ask up to 385. Each answer is one `try_at` measured, so a size
/// called fitting fits.
fn largest_fitting(text_size: f64, mut try_at: impl FnMut(f64) -> (Fitted, bool)) -> Fitted {
    let least = *LAYOUT_TEXT_SIZE.start();
    let most_steps = ((*LAYOUT_TEXT_SIZE.end() - least) / LEGEND_FIT_STEP_PT).ceil();
    let first = try_at(text_size);
    let steps = ((text_size - least) / LEGEND_FIT_STEP_PT).ceil().clamp(0.0, most_steps) as usize;
    if first.1 || steps == 0 {
        return first.0;
    }
    let (last, last_fits) = try_at(least);
    // The size `low` steps down doesn't fit; `high` steps down, the best so far, does.
    let (mut low, mut high, mut best) = (0, steps, last);
    if last_fits {
        while high - low > 1 {
            let middle = low + (high - low) / 2;
            let (f, fits) = try_at((text_size - LEGEND_FIT_STEP_PT * middle as f64).max(least));
            if fits {
                (high, best) = (middle, f);
            } else {
                low = middle;
            }
        }
    }
    best
}

/// The row each column of the rows `m` in `columns` columns starts at as the page draws them: as
/// even as they go when that is no taller than `room_h` points, the `tallest` of those even columns
/// ([`balanced`]); else each filled in turn to `room_h`, the last rows left out ([`flow`]).
pub(super) fn drawn_starts(m: Measured, columns: usize, tallest: f32, room_h: f32) -> Vec<usize> {
    flow(m, columns, tallest.min(room_h)).0
}

/// How wide the rows `m` are in the columns starting at `starts`, side by side with their gaps, in
/// points: the widest of each column ([`widest`]).
pub(super) fn across(m: Measured, starts: &[usize]) -> f32 {
    m.parts(starts).map(widest).sum::<f32>() + LEGEND_COLUMN_GAP * m.size * starts.len().saturating_sub(1) as f32
}

/// A legend's rows `m` laid into at most `columns` columns as evenly as they go ([`balanced`]): how
/// wide they are side by side with their gaps, and how tall the tallest, in points.
pub(super) fn measure(m: Measured, columns: usize) -> (f32, f32) {
    #[cfg(test)]
    tests::LAYOUTS_MEASURED.with_borrow_mut(|n| {
        if let Some(n) = n {
            n.push(columns);
        }
    });
    let (starts, height) = balanced(m, columns);
    (across(m, &starts), height)
}

/// Every class of a layer, whatever a frame draws of it: a classified raster's (#1679), or its
/// renderer's with the sizes of its points after them.
fn every_class(l: &rustgis_map::Layer) -> Vec<LegendEntry> {
    if l.is_raster() { raster_classes(l, true).unwrap_or_default() } else { l.with_size_legend(l.renderer.legend()) }
}

/// The labels of every class row a legend can list for a layer, those a legend can give its own
/// text to (#3187): its classes, its missing-value row (drawn only under `show_no_data`) and its
/// diagram's categories. Empty for a layer with none of these, whose row bears its name.
pub fn class_row_labels(l: &rustgis_map::Layer) -> Vec<String> {
    if !legend_lists(l) {
        return Vec::new();
    }
    let mut entries = every_class(l);
    entries.extend(l.renderer.no_data_legend(&l.dataset, None));
    if let Some(d) = &l.diagram {
        entries.extend(d.legend(l.geometry_type()));
    }
    entries.into_iter().map(|e| e.label).collect()
}

/// The rows of the legend of a frame's map. With `drawn` (from [`shown_legends`]) it shows what the
/// frame draws: only the layers listed there, with their classes. Without, every class of every
/// visible layer. `opts` leaves out `<all other values>` and the layers it names, either way, and
/// without `names` the name over a layer's classes or its legend picture: a layer drawn in one
/// symbol keeps its row, whose name labels its swatch.
pub(super) fn legend_rows<'a>(map: &'a Map, drawn: Option<&[(u32, Vec<LegendEntry>)]>, opts: &LegendOptions<'a>) -> Vec<LegendRow<'a>> {
    legend_rows_missing(map, drawn, opts).0
}

/// [`legend_rows`], and whether a WMS or WMTS legend picture was still on its way when the lookup
/// that makes its rows was made (the row is then left out).
pub(super) fn legend_rows_missing<'a>(
    map: &'a Map,
    drawn: Option<&[(u32, Vec<LegendEntry>)]>,
    opts: &LegendOptions<'a>,
) -> (Vec<LegendRow<'a>>, bool) {
    let mut missing = false;
    let excluded = |name: &str| opts.exclude.iter().any(|n| n.eq_ignore_ascii_case(name));
    let mut rows = Vec::new();
    // In the order the map draws them and the Layers pane lists them, top first.
    for item in map.stack() {
        let l = match item {
            rustgis_map::Drawn::Layer(i) => match map.layers.get(i) {
                Some(l) => l,
                None => continue,
            },
            // A WMS or WMTS layer's legend is its service's picture, whatever the frame draws of it.
            rustgis_map::Drawn::Tile(i) => {
                if let Some(t) = map.tiles.get(i).filter(|t| tile_listed(t) && !excluded(&t.name))
                    && let Some(p) = tile_legend(t, opts.scale, &mut missing)
                {
                    if opts.names {
                        rows.push(LegendRow::Head(opts.shown(&t.name)));
                    }
                    rows.push(LegendRow::Picture(p));
                }
                continue;
            }
        };
        if !(map.shows(l) && legend_lists(l) && !excluded(&l.name)) {
            continue;
        }
        let mut entries = match drawn {
            Some(d) => match d.iter().find(|(id, _)| *id == l.id) {
                Some((_, entries)) => entries.clone(),
                None => continue,
            },
            None => every_class(l),
        };
        if drawn.is_none() && opts.show_no_data {
            entries.extend(l.renderer.no_data_legend(&l.dataset, None));
        }
        // The fallback row only (a class that happens to bear its name stays).
        if opts.hide_other && entries.iter().any(|e| e.fallback) {
            entries.retain(|e| !e.fallback);
            // Nothing left to show for this layer: leave it out rather than draw its plain swatch
            // (unless a diagram lists its categories below).
            if entries.is_empty() && l.diagram.is_none() {
                continue;
            }
        }
        // A layer with a diagram lists its categories under its classes.
        if let Some(d) = &l.diagram {
            entries.extend(d.legend(l.geometry_type()));
        }
        if entries.is_empty() {
            // A Unique Values layer that lists nothing draws nothing visible (#207); a raster has
            // no symbol of its own to show.
            if !l.is_raster() && !matches!(l.renderer, rustgis_map::Renderer::Unique { .. }) {
                rows.push(LegendRow::Plain(l, opts.shown(&l.name)));
            }
            continue;
        }
        if opts.names {
            rows.push(LegendRow::Head(opts.shown(&l.name)));
        }
        // The legend's own text for a class, by the label the layer gives it (#3187).
        let own = opts.class_labels.of(&l.name);
        rows.extend(entries.into_iter().map(|mut en| {
            if let Some(text) = own.and_then(|o| o.get(&en.label)) {
                en.label.clone_from(text);
            }
            LegendRow::Class(l, en)
        }));
    }
    (rows, missing)
}

/// The legend of a frame's map: its title, and its rows ([`legend_rows`]) under it in `columns`
/// columns, each as wide as its widest row. As even as they go in the height the box leaves; in
/// less, each is filled in turn and the last rows are left out. In a box too narrow they close up,
/// so that none is left out for it: long labels run into the next, and the last column's past the
/// box, as a long label does in one column. A `fit` other than manual first picks the columns and
/// text size its rows are drawn at in the room the title leaves ([`fit_legend`]); the title keeps
/// the legend's own size, and the rows are then drawn as a manual legend of those. Either way its
/// rows' point symbols are measured once, and their heights and texts once a size.
///
/// Whether a picture of its rows was still on its way, and, under a fit other than manual, what
/// the fit drew them at ([`Fitted`]), which an export says when they still don't fit
/// ([`unfitted_legends`]).
pub(super) fn legend_missing(
    pm: &mut Canvas,
    r: Rect,
    map: &Map,
    (title, columns, fit): (&str, usize, LegendFit),
    drawn: Option<&[(u32, Vec<LegendEntry>)]>,
    opts: LegendOptions,
    ink: Rgba,
    ppp: f64,
) -> (bool, Option<Fitted>) {
    let ppp = ppp as f32;
    let mut y = r.y();
    if !title.is_empty() {
        let line = LEGEND_ROW * opts.size * ppp;
        text::text_at(pm, title, r.x(), y + line / 2.0, LEGEND_TITLE_PT * opts.size * 1.33 * ppp, ink, true, text::Align::Left, false);
        y += line * LEGEND_TITLE_ROWS;
    }
    let (rows, missing) = legend_rows_missing(map, drawn, &opts);
    let sides = symbol_sides(&rows, opts.scale);
    let room_h = (r.bottom() - y) / ppp;
    // As set; or the columns and size a fit picks, its rows measured at that size already and laid
    // in the columns its width was measured in.
    let (opts, columns, sizes, starts, fitted) = match fit.kept() {
        LegendFit::Manual | LegendFit::Invalid => {
            let columns = legend_columns_in_range(columns);
            let sizes = RowSizes::of(&rows, &sides, opts.size);
            let m = Measured::new(&rows, &sides, &sizes);
            let starts = drawn_starts(m, columns, balanced(m, columns).1, room_h);
            (opts, columns, sizes, starts, None)
        }
        _ => {
            let (fitted, sizes) = fit_measured(&rows, &sides, (columns, fit), (r.width() / ppp, room_h), opts.text_size);
            let starts = fitted.starts.clone();
            (LegendOptions { size: fitted.size, text_size: fitted.text_size, ..opts }, fitted.columns, sizes, starts, Some(fitted))
        }
    };
    let m = Measured::new(&rows, &sides, &sizes);
    let gap = LEGEND_COLUMN_GAP * opts.size;
    let across = across(m, &starts) * ppp;
    let close_up = if across > r.width() { r.width() / across } else { 1.0 };
    let mut x = r.x();
    for (k, part) in m.parts(&starts).enumerate() {
        // The last has the rest of the box's width.
        let right = if k + 1 < starts.len() { x + widest(part) * ppp * close_up } else { r.right() };
        let Some(column) = Rect::from_ltrb(x, y, right, r.bottom()) else { break };
        // The room left of it that holds nothing: none at the box's edge, the gap between columns
        // as drawn after it.
        let lead = if k == 0 { 0.0 } else { gap * ppp * close_up };
        legend_column(pm, column, part, (columns > 1, lead), map, &opts, ink, ppp);
        x = right + gap * ppp * close_up;
    }
    (missing, fitted)
}

/// [`legend_missing`], for a legend whose missing pictures nobody asks about.
#[cfg(test)]
pub(super) fn legend(
    pm: &mut Canvas,
    r: Rect,
    map: &Map,
    (title, columns): (&str, usize),
    drawn: Option<&[(u32, Vec<LegendEntry>)]>,
    opts: LegendOptions,
    ppp: f64,
) {
    legend_missing(pm, r, map, (title, columns, LegendFit::Manual), drawn, opts, INK, ppp);
}

/// A column of a legend: its rows `m` from the top of `r`, as many as fit, by their measures (their
/// symbols' sides and heights, measured once). With `name_with_class`, a layer's name only with
/// room for the class under it, as [`flow`] keeps them together. `lead` pixels left of it hold
/// nothing, which its swatches may reach into.
pub(super) fn legend_column(
    pm: &mut Canvas,
    r: Rect,
    m: Measured,
    (name_with_class, lead): (bool, f32),
    map: &Map,
    opts: &LegendOptions,
    dark: Rgba,
    ppp: f32,
) {
    let mut y = r.y();
    let size = opts.size;
    let line = LEGEND_ROW * size * ppp;
    let sw = swatch_column(m.sides.iter().copied(), size) * ppp;
    // A line's or polygon's swatch, across the column of swatches.
    let across = LEGEND_SWATCH * size * ppp;
    let mut rows = m.rows.iter().zip(m.heights).peekable();
    while let Some((row, &row_height)) = rows.next() {
        if let LegendRow::Picture(p) = row {
            match &**p {
                rustgis_tiles::cache::Payload::Image(pic) => match legend_picture(pm, r, y, pic, ppp * size) {
                    Some((h, true)) => y += h,
                    // Cut at the bottom, or no room for it.
                    _ => break,
                },
                _ => continue,
            }
            continue;
        }
        // The rectangle's own bottom, which a sum of its top and height can miss by a rounding,
        // and a thousandth of a row's slack: a legend sized to its rows keeps the last one.
        let height = row_height * ppp;
        if y + height > r.bottom() + line * 1e-3 {
            break;
        }
        // A WMS or WMTS layer's name only with room for some of its picture under it.
        if let (LegendRow::Head(_), Some((LegendRow::Picture(p), _))) = (&row, rows.peek())
            && let rustgis_tiles::cache::Payload::Image(pic) = &**p
            && y + line + picture_scale(r, pic, ppp * size) > r.bottom() + line * 1e-3
        {
            break;
        }
        if let (true, LegendRow::Head(_), Some(&(next, next_height))) = (name_with_class, &row, rows.peek())
            && matches!(next, LegendRow::Class(..))
            && y + height + next_height * ppp > r.bottom() + line * 1e-3
        {
            break;
        }
        match row {
            LegendRow::Plain(l, name) => {
                let s = l.renderer.symbol_for(&l.dataset, 0);
                let sym = (&s, l.line_strokes.as_slice(), l.fill_layers.as_slice());
                // Its room: left of the column, and the gap before its name.
                let room = (lead, LEGEND_INDENT * size * ppp);
                swatch(pm, (r.x(), y, sw, height), l, sym, map.layer_opacity(l), opts.scale, (ppp, across, room));
                let (x, pt) = (r.x() + sw + LEGEND_INDENT * size * ppp, LEGEND_NAME_PT * size * 1.33 * ppp);
                text::text_at(pm, name, x, y + height / 2.0, pt, dark, false, text::Align::Left, false);
            }
            LegendRow::Head(name) => {
                text::text_at(pm, name, r.x(), y + line / 2.0, LEGEND_NAME_PT * size * 1.33 * ppp, dark, true, text::Align::Left, false);
            }
            LegendRow::Class(l, en) => {
                swatch(
                    pm,
                    (r.x() + LEGEND_INDENT * size * ppp, y, sw, height),
                    l,
                    (&en.symbol, row_strokes(l, en), row_fills(l, en)),
                    map.layer_opacity(l),
                    opts.scale,
                    // Its room: its indent and what lies left of the column, and the gap before its label.
                    (ppp, across, (lead + LEGEND_INDENT * size * ppp, (LEGEND_LABEL_INDENT - LEGEND_INDENT) * size * ppp)),
                );
                text::text_at(
                    pm,
                    &en.label,
                    r.x() + sw + LEGEND_LABEL_INDENT * size * ppp,
                    y + height / 2.0,
                    LEGEND_LABEL_PT * size * 1.33 * ppp,
                    dark,
                    false,
                    text::Align::Left,
                    false,
                );
            }
            LegendRow::Picture(_) => {}
        }
        y += height;
    }
}

/// The pixels a legend picture's pixel takes in the legend `r`: a pixel a point, narrowed to fit
/// across after its indent.
pub(super) fn picture_scale(r: Rect, pic: &Pixmap, ppp: f32) -> f32 {
    ppp.min((r.right() - r.x() - 6.0 * ppp).max(0.0) / pic.width().max(1) as f32)
}

/// Draws a WMS or WMTS layer's legend picture at `y` in the legend `r`, indented as a layer's
/// classes, a pixel a point, narrowed to fit across ([`picture_scale`]). One taller than the room
/// left is cut at the bottom. The height drawn and whether it all fitted; `None` when none of it
/// did.
pub(super) fn legend_picture(pm: &mut Canvas, r: Rect, y: f32, pic: &Pixmap, ppp: f32) -> Option<(f32, bool)> {
    let x = r.x() + 6.0 * ppp;
    let k = picture_scale(r, pic, ppp);
    let room = r.bottom() - y;
    // The source rows that fit, a thousandth of a row's slack, as the rows have.
    let rows = if k > 0.0 { ((room + LEGEND_ROW * ppp * 1e-3) / k).floor().min(pic.height() as f32) } else { 0.0 };
    if rows < 1.0 {
        return None;
    }
    let shown;
    let src = if (rows as u32) < pic.height() {
        shown = tiny_skia::IntRect::from_xywh(0, 0, pic.width(), rows as u32).and_then(|c| pic.clone_rect(c))?;
        &shown
    } else {
        pic
    };
    pm.draw_pixmap(
        0,
        0,
        src.as_ref(),
        &PixmapPaint { quality: tiny_skia::FilterQuality::Bilinear, ..PixmapPaint::default() },
        Transform::from_row(k, 0.0, 0.0, k, x, y),
        None,
    );
    Some((rows * k, (rows as u32) == pic.height()))
}

/// The size a legend with no title takes in one column, in two and so on, in mm (width, height):
/// its rows as [`legend`] draws them, the classes a frame shows at the renderer's scale denominator
/// `scale` (as the layout finds it for a frame, a complete source read through as [`render`] reads
/// it), or every class with `all_classes`. For sizing a legend to its content. `None` when a
/// listed layer's complete source can't be read; one in `exclude` isn't read. A WMS or WMTS
/// layer's legend picture still on its way isn't waited for, nor counted
/// ([`tile_legends_pending`] says whether one is). Its layers' rows are measured by the `labels`
/// it shows for them, else their names.
pub fn legend_sizes_mm(
    map: &mut Map,
    scale: f64,
    all_classes: bool,
    hide_other: bool,
    (exclude, labels): (&[String], &BTreeMap<String, String>),
) -> Option<[(f64, f64); MAX_LEGEND_COLUMNS]> {
    let text_size = rustgis_map::layout::LEGEND_TEXT_PT;
    legend_sizes_with_no_data_mm(map, scale, all_classes, hide_other, (false, text_size), (exclude, labels))
}

/// Sizes of an optional no-data legend whose text is `text_size` points (#2581), using the same
/// rows and filters as the drawing, its layers' names shown.
pub fn legend_sizes_with_no_data_mm(
    map: &mut Map,
    scale: f64,
    all_classes: bool,
    hide_other: bool,
    (show_no_data, text_size): (bool, f64),
    (exclude, labels): (&[String], &BTreeMap<String, String>),
) -> Option<[(f64, f64); MAX_LEGEND_COLUMNS]> {
    legend_sizes_in_columns_mm(map, scale, all_classes, hide_other, (show_no_data, text_size), (exclude, labels))
}

/// [`legend_sizes_with_no_data_mm`] in one to `N` columns alone, `N` at most
/// [`MAX_LEGEND_COLUMNS`] (checked as it is built): for a caller that lays a legend out in no more
/// columns, which then measures no more.
pub fn legend_sizes_in_columns_mm<const N: usize>(
    map: &mut Map,
    scale: f64,
    all_classes: bool,
    hide_other: bool,
    (show_no_data, text_size): (bool, f64),
    (exclude, labels): (&[String], &BTreeMap<String, String>),
) -> Option<[(f64, f64); N]> {
    // Measured with no class labels: publication's legends have none, and the public sizers that
    // come here (`legend_sizes_mm`, `legend_sizes_with_no_data_mm`) take none. A saved legend is
    // measured through `LegendAsked`, which carries its own.
    let class_labels = &ClassLabels::NONE;
    let asked = LegendAsked { all_classes, hide_other, show_no_data, layer_names: true, text_size, exclude, labels, class_labels };
    sizes_asked_mm(map, scale, &asked)
}

/// What a legend element asks of its rows, for measuring them as they are drawn: every class or
/// those its frame draws, without `<all other values>`, with its no-data classes, with its layers'
/// names, its text size in points, the layers it leaves out and the labels it shows for them.
pub struct LegendAsked<'a> {
    pub all_classes: bool,
    pub hide_other: bool,
    pub show_no_data: bool,
    pub layer_names: bool,
    pub text_size: f64,
    pub exclude: &'a [String],
    pub labels: &'a BTreeMap<String, String>,
    pub class_labels: &'a ClassLabels,
}

/// The columns and text size a legend's fit draws its rows at, and whether they are still wider
/// or taller than its box there ([`legend_fitted_mm`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LegendFitted {
    pub columns: usize,
    pub text_size: f64,
    pub too_wide: bool,
    pub too_tall: bool,
}

/// The rows of the legend `asked` of `map` at the renderer's scale denominator `scale`, as the
/// drawing makes them, given to `with`. `None` when a listed layer's complete source can't be read.
fn asked_rows<T>(map: &mut Map, scale: f64, asked: &LegendAsked, with: impl FnOnce(&[LegendRow]) -> T) -> Option<T> {
    let LegendAsked { all_classes, hide_other, show_no_data, layer_names, text_size, exclude, labels, class_labels } = *asked;
    let drawn =
        if all_classes { None } else { Some(complete_shown_legends(map, scale, show_no_data, exclude, &mut None, &Env::default())?) };
    let size = legend_size(text_size);
    let opts = LegendOptions { show_no_data, hide_other, names: layer_names, exclude, labels, class_labels, scale, size, text_size };
    Some(with(&legend_rows(map, drawn.as_deref(), &opts)))
}

/// [`legend_sizes_mm`] of the legend `asked`: its size in one column, in two and so on, in mm.
pub fn legend_sizes_asked_mm(map: &mut Map, scale: f64, asked: &LegendAsked) -> Option<[(f64, f64); MAX_LEGEND_COLUMNS]> {
    sizes_asked_mm(map, scale, asked)
}

/// [`legend_sizes_asked_mm`] in one to `N` columns alone, `N` at most [`MAX_LEGEND_COLUMNS`]: a
/// larger `N` doesn't build.
fn sizes_asked_mm<const N: usize>(map: &mut Map, scale: f64, asked: &LegendAsked) -> Option<[(f64, f64); N]> {
    const { assert!(N <= MAX_LEGEND_COLUMNS) };
    asked_measures(map, scale, asked, |m| std::array::from_fn(|i| measure_mm(m, i + 1)))
}

/// [`legend_sizes_asked_mm`] in `columns` columns alone (taken into range as a legend takes them,
/// [`legend_columns_in_range`]), in mm: what a legend laid out in them needs, no other column
/// count measured.
pub fn legend_size_asked_mm(map: &mut Map, scale: f64, asked: &LegendAsked, columns: usize) -> Option<(f64, f64)> {
    let columns = legend_columns_in_range(columns);
    asked_measures(map, scale, asked, |m| measure_mm(m, columns))
}

/// The rows of the legend `asked` ([`asked_rows`]) measured at its text size, given to `with`.
fn asked_measures<T>(map: &mut Map, scale: f64, asked: &LegendAsked, with: impl FnOnce(Measured<'_, '_>) -> T) -> Option<T> {
    let size = legend_size(asked.text_size);
    asked_rows(map, scale, asked, |rows| {
        let sides = symbol_sides(rows, scale);
        let sizes = RowSizes::of(rows, &sides, size);
        with(Measured::new(rows, &sides, &sizes))
    })
}

/// [`measure`] of `m` in `columns` columns, in mm.
fn measure_mm(m: Measured, columns: usize) -> (f64, f64) {
    let mm = 25.4 / 96.0;
    let (width, height) = measure(m, columns);
    (f64::from(width) * mm, f64::from(height) * mm)
}

/// The columns and text size the legend `asked` of `map` at the renderer's scale denominator
/// `scale` is drawn at under its `fit` from its `columns`, in a room of `room_mm` (its box less its
/// title, as [`legend_title_mm`] gives it), and whether it is still wider or taller than that: the
/// drawing's own rule ([`fit_legend`]). `None` when a listed layer's complete source can't be read.
pub fn legend_fitted_mm(
    map: &mut Map,
    scale: f64,
    asked: &LegendAsked,
    (columns, fit): (usize, LegendFit),
    room_mm: (f64, f64),
) -> Option<LegendFitted> {
    let room = ((room_mm.0 * PT_PER_MM) as f32, (room_mm.1 * PT_PER_MM) as f32);
    asked_rows(map, scale, asked, |rows| {
        let f = fit_legend(rows, (columns, fit), room, (scale, asked.text_size));
        LegendFitted { columns: f.columns, text_size: f.text_size, too_wide: f.too_wide, too_tall: f.too_tall }
    })
}

/// The width in mm [`legend`] draws `title` in, in a legend whose text is `text_size` points
/// (#3095).
pub fn legend_title_width_mm(title: &str, text_size: f64) -> f64 {
    f64::from(text::measure(title, LEGEND_TITLE_PT * legend_size(text_size), true)) / PT_PER_MM
}

/// The height in mm [`legend`] gives a title above its rows, which [`legend_sizes_mm`] leaves out,
/// in a legend whose text is `text_size` points (#2581).
pub fn legend_title_mm(text_size: f64) -> f64 {
    f64::from(LEGEND_ROW * legend_size(text_size)) * f64::from(LEGEND_TITLE_ROWS) * 25.4 / 96.0
}
