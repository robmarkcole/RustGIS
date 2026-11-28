//! Legends and colorbars.

use super::*;

pub(in crate::cmd::layout) const ONLY_LEGEND_COLUMNS: &str = "only legends have columns";
/// A legend's layer names given to another element.
pub(in crate::cmd::layout) const ONLY_LEGEND_NAMES: &str = "only legends take showLayerNames";
/// A fit given to an element that is neither a legend nor a scale bar, each of which reads `fit`
/// as its own.
pub(in crate::cmd::layout) const ONLY_FIT: &str = "only legends and scale bars take fit";
/// Refuse the optional no-data key on an element that cannot display it.
pub(in crate::cmd::layout) const ONLY_LEGEND_NO_DATA: &str = "only legends take showNoData";
pub(in crate::cmd::layout) const ONLY_LEGEND_LABELS: &str = "only legends label layers";

/// A box sized to its content, less the padding again, can come out a rounding error short of it:
/// that still fits.
pub(in crate::cmd::layout) const LEGEND_FIT_MM: f64 = 1e-6;

/// Whether a legend's rows `(w, h)` in mm are wider and taller than their room: none are when it
/// has no rows, in a box shorter than its title too.
pub(in crate::cmd::layout) fn overflows((w, h): (f64, f64), (room_w, room_h): (f64, f64)) -> (bool, bool) {
    (h > 0.0 && w > room_w + LEGEND_FIT_MM, h > 0.0 && h > room_h + LEGEND_FIT_MM)
}

/// What a legend drawn in `columns` columns loses in its box when its rows are `too_wide` or
/// `too_tall` for it, each said once. A manual legend says it as it always has; a fit that still
/// leaves them out says what it tried: the smallest text, the most columns its box takes across,
/// or both.
pub(in crate::cmd::layout) fn legend_fit_warnings(fit: LegendFit, columns: usize, (too_wide, too_tall): (bool, bool)) -> Vec<String> {
    let smallest = LAYOUT_TEXT_SIZE.start();
    let across = if columns == 1 { "one column".to_string() } else { format!("{columns} columns") };
    let even = match fit.kept() {
        LegendFit::AdjustSize => format!(" even with its text shrunk to {smallest} points"),
        LegendFit::AdjustColumns => format!(" even in {across}"),
        LegendFit::AdjustColumnsAndSize => format!(" even in {across} with its text shrunk to {smallest} points"),
        LegendFit::Manual | LegendFit::Invalid => String::new(),
    };
    let mut said = Vec::new();
    if columns > 1 {
        // Its columns close up rather than leave one out.
        if too_wide {
            said.push(format!(
                "the legend's columns are wider than its box{even}: they close up, long labels run into the next, and the last column's past the box"
            ));
        }
        if too_tall {
            said.push(format!("the legend lists more than fits its box{even}: its last rows are left out"));
        }
    } else if too_wide || too_tall {
        said.push(format!("the legend lists more than fits its box{even}: its last rows or the ends of long labels are left out"));
    }
    said
}

/// What a legend of `map` can't key, or keys only in part, for the rasters it shows by classes
/// (#1679), those `exclude` leaves out aside: one warning a kind, with how many and their names.
/// A colour table over more values than a legend can list (over 256, or not whole numbers) gives
/// no class and no key; classes taken from an overview's statistics (a COG read in place), or from
/// a sample of a large raster under a mask, can miss a rare one the map draws, unless
/// `all_classes` lists every class of the renderer.
pub(in crate::cmd::layout) fn raster_key_notices(map: &Map, exclude: &[String], all_classes: bool) -> Vec<String> {
    let shown: Vec<(&str, &rustgis_map::RasterLayer)> = map
        .layers
        .iter()
        .filter(|l| map.shows(l) && !exclude.iter().any(|n| n.eq_ignore_ascii_case(&l.name)))
        .filter_map(|l| Some((l.name.as_str(), l.raster.as_ref()?)))
        .collect();
    let names = |kind: &dyn Fn(&rustgis_map::RasterLayer) -> bool| -> Vec<&str> {
        shown.iter().filter(|(_, r)| kind(r)).map(|(n, _)| *n).collect()
    };
    let unlisted = names(&rustgis_map::RasterLayer::classes_unknown);
    let reduced = names(&|r| r.classes_from_overview(all_classes));
    let sampled = names(&|r| r.classes_from_sample(all_classes));
    let mut said = Vec::new();
    if !unlisted.is_empty() {
        said.push(format!(
            "{} raster(s) drawn by a colour table over more values than a legend can list (over 256, or not whole numbers) have no key in the legend: {}",
            unlisted.len(),
            unlisted.join(", ")
        ));
    }
    if !reduced.is_empty() {
        said.push(format!(
            "{} raster(s) drawn by classes were read through an overview, so the legend lists the classes found there and may miss a rare one the map draws: {}",
            reduced.len(),
            reduced.join(", ")
        ));
    }
    if !sampled.is_empty() {
        said.push(format!(
            "{} raster(s) drawn by classes under a mask have more cells than their masked statistics read, so the legend lists the classes of a sample and may miss a rare one the map draws: {}",
            sampled.len(),
            sampled.join(", ")
        ));
    }
    said
}

/// Says, a kind once, what the legend `id` of the layout `li` can't key ([`raster_key_notices`]),
/// as `layout.publication` does for its own: for a legend added, or told what it leaves out or
/// whether it lists every class.
pub(in crate::cmd::layout) fn say_raster_keys(s: &mut Session, li: usize, id: u32) {
    let Some(l) = s.project.layouts.get(li) else { return };
    let Some(ElementKind::Legend { frame, exclude, all_classes, .. }) = l.element(id).map(|e| &e.kind) else { return };
    let Some(ElementKind::MapFrame { map, .. }) = l.element(*frame).map(|f| &f.kind) else { return };
    let said = s.project.maps.iter().find(|m| m.name.eq_ignore_ascii_case(map)).map(|m| raster_key_notices(m, exclude, *all_classes));
    for said in said.unwrap_or_default() {
        s.warn(said);
    }
}

/// Whether a legend `kind`, after a command with `p`, says what it loses in its box (#2581): the
/// one rule `layout.addElement` and `layout.update` both take. Told when it is given what sets
/// what it needs of its box (its columns, its missing-value row, its fit, its layer names, or its
/// text size, `null` included, which gives the default back) or labels (text chosen for the box),
/// and, in columns, at a text size of its own or under a fit, which can stop fitting, when its box
/// or what it lists changes. A move changes neither.
pub(in crate::cmd::layout) fn tells_legend_fit(kind: &ElementKind, p: &Value) -> bool {
    let ElementKind::Legend { columns, text_size, fit, .. } = kind else { return false };
    let given = |k: &str| p.get(k).is_some_and(|v| !v.is_null());
    let watched = *columns > 1 || *text_size != rustgis_map::layout::LEGEND_TEXT_PT || fit.kept() != LegendFit::Manual;
    let refitted = ["w", "h", "title", "allClasses", "hideOther", "exclude", "labels", "classLabels"].iter().any(|k| given(k));
    ["columns", "showNoData", "labels", "classLabels", "fit", "showLayerNames"].iter().any(|k| given(k))
        || p.get("textSize").is_some()
        || (watched && refitted)
}

/// Says what a legend loses in its box, as `layout.publication` does for its own, or that it can't
/// tell. A manual legend is measured in its own columns at its own size; a fitted one by the
/// drawing's own fit ([`crate::render::layout::legend_fitted_mm`]), in the room its title leaves,
/// so it says nothing when the page draws it whole and what it tried when it can't.
pub(in crate::cmd::layout) fn say_legend_fit(s: &mut Session, li: usize, id: u32) {
    let Some(l) = s.project.layouts.get(li) else { return };
    let Some(e) = l.element(id) else { return };
    let ElementKind::Legend {
        frame,
        title,
        all_classes,
        hide_other,
        show_no_data,
        exclude,
        columns,
        labels,
        text_size,
        fit,
        show_layer_names,
        class_labels,
        ..
    } = &e.kind
    else {
        return;
    };
    let shown = match l.element(*frame).map(|f| &f.kind) {
        Some(ElementKind::MapFrame { map, center, scale, .. }) => Some((map.clone(), *center, *scale)),
        _ => None,
    };
    let room = (e.w, e.h - if title.is_empty() { 0.0 } else { crate::render::layout::legend_title_mm(*text_size) });
    let (all_classes, hide_other, show_no_data, exclude, columns, labels, text_size) =
        (*all_classes, *hide_other, *show_no_data, exclude.clone(), *columns, labels.clone(), *text_size);
    let (fit, layer_names, class_labels) = (fit.kept(), show_layer_names.shown(), class_labels.clone());
    let found = shown
        .and_then(|(name, center, scale)| Some((s.project.maps.iter_mut().find(|m| m.name.eq_ignore_ascii_case(&name))?, center, scale)));
    let Some((map, center, scale)) = found else {
        s.warn("the legend's frame shows no map of this project, so the legend lists nothing");
        return;
    };
    let at = crate::render::layout::frame_render_scale(map, center, scale);
    let asked = crate::render::layout::LegendAsked {
        all_classes,
        hide_other,
        show_no_data,
        layer_names,
        text_size,
        exclude: &exclude,
        labels: &labels,
        class_labels: &class_labels,
    };
    let measured = if fit == LegendFit::Manual {
        let n = rustgis_map::layout::legend_columns_in_range(columns);
        crate::render::layout::legend_size_asked_mm(map, at, &asked, n).map(|size| (n, overflows(size, room)))
    } else {
        crate::render::layout::legend_fitted_mm(map, at, &asked, (columns, fit), room).map(|f| (f.columns, (f.too_wide, f.too_tall)))
    };
    let Some((drawn, verdict)) = measured else {
        s.warn("a layer's source couldn't be read through, so the legend's fit in its box wasn't checked; exporting says why");
        return;
    };
    for said in legend_fit_warnings(fit, drawn, verdict) {
        s.warn(said);
    }
}

/// Says what the legends under a fit that an export drew still lose in their boxes
/// ([`rustgis_render::layout::unfitted_legends`]), in the words setting the fit gave
/// ([`legend_fit_warnings`]): a fit can stop succeeding after it is set, as when a layer is added to
/// its map. One notice a reason, with how many legends it is true of and which, by name (shortened,
/// as [`crate::project::short_name`] does) and id, the first ten, as a scale bar's notices count
/// theirs. A manual legend is not listed, so its export says what it did.
pub(in crate::cmd::layout) fn say_unfitted_legends(s: &mut Session, unfitted: &[rustgis_render::layout::UnfittedLegend]) {
    let listed: Vec<_> = unfitted.iter().map(|l| (None, l.clone())).collect();
    say_unfitted_legends_in(s, &listed);
}

/// [`say_unfitted_legends`] of legends each in the layout it names (by its index and name), when
/// one does: a report draws several layouts, whose legends' ids can be the same, and names each
/// legend's layout (shortened as its name is), `"Legend" (3) in "Atlas"`. A legend is the same
/// legend by its layout's index and its id: listed again (another page of a series, cut another
/// way that a reason's words don't tell apart), it is named and counted once under that reason.
pub(in crate::cmd) fn say_unfitted_legends_in(
    s: &mut Session,
    unfitted: &[(Option<(usize, String)>, rustgis_render::layout::UnfittedLegend)],
) {
    // Each reason, in the order first met, with the legends it is true of, each once.
    let mut said: Vec<(String, Vec<((Option<usize>, u32), String)>)> = Vec::new();
    for (layout, l) in unfitted {
        let mut named = format!("\"{}\" ({})", crate::project::short_name(&l.name), l.id);
        if let Some((_, layout)) = layout {
            named.push_str(&format!(" in \"{}\"", crate::project::short_name(layout)));
        }
        let legend = (layout.as_ref().map(|(li, _)| *li), l.id);
        for reason in legend_fit_warnings(l.fit, l.columns, (l.too_wide, l.too_tall)) {
            match said.iter_mut().find(|(r, _)| *r == reason) {
                Some((_, legends)) if legends.iter().any(|(at, _)| *at == legend) => {}
                Some((_, legends)) => legends.push((legend, named.clone())),
                None => said.push((reason, vec![(legend, named.clone())])),
            }
        }
    }
    for (reason, legends) in said {
        let n = legends.len();
        let mut names = legends.into_iter().take(10).map(|(_, named)| named).collect::<Vec<_>>().join(", ");
        if n > 10 {
            names.push_str(&format!(" and {} more", n - 10));
        }
        let notice = if n == 1 { format!("Legend {names}: {reason}") } else { format!("{n} legends ({names}): {reason}") };
        s.warn(notice);
    }
}

/// What a scale bar is given that changes its labels (#2581): its text size (`null` included, which
/// gives the default back), its units and divisions, which change what they say, and its details,
/// which change its rows, its labels and the room they have (`null` included: a unit label given
/// back is its abbreviation again).
pub(in crate::cmd::layout) const BAR_FIT_KEYS: [&str; 10] = [
    "textSize",
    "units",
    "divisions",
    "subdivisions",
    "divisionsBeforeZero",
    "labelPosition",
    "unitLabel",
    "unitLabelPosition",
    "decimals",
    "fit",
];

/// The `"columns"` a legend is given, refused outside 1 to [`MAX_LEGEND_COLUMNS`]: the one reader
/// the commands and the `layout-legend-fit` capability check share.
pub(crate) fn legend_columns_param(p: &Value) -> Result<Option<usize>, String> {
    let Some(v) = p.get("columns").filter(|v| !v.is_null()) else { return Ok(None) };
    v.as_u64()
        .and_then(|n| usize::try_from(n).ok())
        .filter(|n| (1..=MAX_LEGEND_COLUMNS).contains(n))
        .map(Some)
        .ok_or_else(|| format!("columns is a whole number from 1 to {MAX_LEGEND_COLUMNS}"))
}

/// The `"fit"` a legend is given, by name in any case: the one reader the commands and the
/// `layout-legend-fit` capability check share. A scale bar reads its own `fit` (`restyle`).
pub(crate) fn legend_fit_param(p: &Value) -> Result<Option<LegendFit>, String> {
    given_choice(p, "fit", LegendFit::from_name, "manual, adjustSize, adjustColumns or adjustColumnsAndSize")
}

/// The `"showLayerNames"` a legend is given, `true` or `false` only (`null` is not given): the one
/// reader the commands and the `layout-legend-fit` capability check share.
pub(crate) fn legend_names_param(p: &Value) -> Result<Option<bool>, String> {
    match p.get("showLayerNames") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(shown)) => Ok(Some(*shown)),
        Some(_) => Err("showLayerNames is true or false".into()),
    }
}

/// What a colorbar with no layer of its own on `frame` says when the frame's map shows no raster
/// with a colour scale for it but one drawn by classes (#1679): it draws an empty placeholder; the
/// legend lists those classes, unless a colour table has more values than it can list. `None`
/// otherwise. Said when the colorbar is added, or set back to no layer.
pub(in crate::cmd::layout) fn colorbar_without_raster(s: &Session, li: usize, frame: u32) -> Option<String> {
    let l = s.project.layouts.get(li)?;
    let Some(ElementKind::MapFrame { map, .. }) = l.element(frame).map(|f| &f.kind) else { return None };
    let m = s.project.maps.iter().find(|m| m.name.eq_ignore_ascii_case(map))?;
    if rustgis_map::layout::colorbar_layer(m, None).is_some() {
        return None;
    }
    // Those the legend lists, and colour tables over more values than it can list, which nothing keys.
    let (mut listed, mut unlisted) = (Vec::new(), Vec::new());
    for l in m.layers.iter().filter(|l| m.shows(l)) {
        match l.raster.as_ref().filter(|r| r.style.renderer.draws_classes()) {
            Some(r) if r.classes_unknown() => unlisted.push(l.name.as_str()),
            Some(_) => listed.push(l.name.as_str()),
            None => {}
        }
    }
    let mut why = Vec::new();
    if !listed.is_empty() {
        why.push(format!(
            "{} is drawn by classes, which a legend lists rather than a colorbar (give \"layer\" to show its colours on one anyway)",
            listed.join(", ")
        ));
    }
    if !unlisted.is_empty() {
        why.push(format!(
            "{} draws a colour table over more values than a legend or a colorbar can list (over 256, or not whole numbers), so nothing keys it",
            unlisted.join(", ")
        ));
    }
    (!why.is_empty())
        .then(|| format!("the colorbar has no raster with a colour scale to show, so it draws an empty placeholder: {}", why.join("; ")))
}

/// The raster layer a colorbar on `frame` shows: `name` checked against the frame's map, or the
/// map's top visible raster with a colour scale, not drawn by classes
/// ([`rustgis_map::layout::colorbar_layer`]; `None` when the map has none yet).
pub(in crate::cmd::layout) fn colorbar_layer(
    input: &impl crate::cmd::InputSource,
    l: &Layout,
    frame: u32,
    name: Option<&str>,
) -> Result<Option<String>, String> {
    let context = input.input_context();
    let s = &context;
    let map = match l.element(frame).map(|e| &e.kind) {
        Some(ElementKind::MapFrame { map, .. }) => map.clone(),
        _ => return Err("no such frame".into()),
    };
    let m = s.project.maps.iter().find(|m| m.name.eq_ignore_ascii_case(&map)).ok_or_else(|| format!("no map '{map}'"))?;
    match name.map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => match rustgis_map::layout::colorbar_layer(m, Some(n)) {
            Some(layer) => Ok(Some(layer.name.clone())),
            None => Err(format!("no raster layer '{n}' in {map}")),
        },
        None => Ok(rustgis_map::layout::colorbar_layer(m, None).map(|l| l.name.clone())),
    }
}

/// A legend's `exclude` from `p`: the names of layers in its frame's map, as the map spells them;
/// `None` when not given. Not a list of names, or a name the map hasn't, is an error.
pub(in crate::cmd::layout) fn legend_exclude(
    input: &impl crate::cmd::InputSource,
    l: &Layout,
    frame: u32,
    p: &Value,
) -> Result<Option<Vec<String>>, String> {
    let Some(v) = p.get("exclude").filter(|v| !v.is_null()) else { return Ok(None) };
    excluded_layers(legend_map(input, l, frame)?, v).map(Some)
}

/// The layers of `map` that `v`, a legend's `exclude`, names, as the map spells them, each once.
pub(in crate::cmd::layout) fn excluded_layers(map: &Map, v: &Value) -> Result<Vec<String>, String> {
    let names = v.as_array().ok_or("exclude is a list of layer names")?;
    if names.len() > rustgis_map::layout::MAX_LEGEND_EXCLUDED {
        return Err(format!("a legend leaves out at most {} layers", rustgis_map::layout::MAX_LEGEND_EXCLUDED));
    }
    let mut out: Vec<String> = Vec::new();
    for n in names {
        let n = n.as_str().ok_or("exclude is a list of layer names")?;
        let name = legend_layer(map, n)?;
        if !out.iter().any(|o| o.eq_ignore_ascii_case(name)) {
            out.push(name.clone());
        }
    }
    Ok(out)
}

/// The map a legend's frame shows.
pub(in crate::cmd::layout) fn legend_map<'m>(input: &'m impl crate::cmd::InputSource, l: &Layout, frame: u32) -> Result<&'m Map, String> {
    let context = input.input_context();
    let s = &context;
    let map_name = match l.element(frame).map(|e| &e.kind) {
        Some(ElementKind::MapFrame { map, .. }) => map,
        _ => return Err("the legend's frame isn't a map frame".into()),
    };
    s.project.maps.iter().find(|m| m.name.eq_ignore_ascii_case(map_name)).ok_or_else(|| format!("no map '{map_name}'"))
}

/// The name of the layer `n` (any case) of a legend's `map`, as the map spells it: one of its
/// layers, or a WMS or WMTS layer's (whose legend is its service's picture). `n` is looked up as
/// given first, as a layer's name may begin or end with a space, then with its ends trimmed. One
/// the map hasn't is an error.
pub(in crate::cmd::layout) fn legend_layer<'m>(map: &'m Map, n: &str) -> Result<&'m String, String> {
    let named = |n: &str| {
        map.layer_by_name(n).map(|l| &l.name).or_else(|| map.tiles.iter().find(|t| t.name.eq_ignore_ascii_case(n)).map(|t| &t.name))
    };
    named(n).or_else(|| named(n.trim())).ok_or_else(|| format!("no layer '{n}' in map '{}'", map.name))
}

/// The shape a legend's `labels` takes.
pub(in crate::cmd::layout) const LABELS_SHAPE: &str = r#"labels is an object of layer names and labels: {"<layer>": "<label>" | null}"#;

/// A legend's `labels` from `p` (#1654): each layer's name, as its frame's map spells it, with the
/// label to show for it, or `None` to show its name again (a `null` or blank label). `None` when
/// not given; an empty list for `{}`, which shows every layer's name again. Not an object of
/// labels, a name the map hasn't, or a layer named twice is an error.
pub(in crate::cmd::layout) fn legend_labels(
    input: &impl crate::cmd::InputSource,
    l: &Layout,
    frame: u32,
    p: &Value,
) -> Result<Option<Vec<(String, Option<String>)>>, String> {
    let Some(v) = p.get("labels").filter(|v| !v.is_null()) else { return Ok(None) };
    let given = v.as_object().ok_or(LABELS_SHAPE)?;
    if given.len() > MAX_LEGEND_LABELS {
        return Err(format!("a legend can label at most {MAX_LEGEND_LABELS} layers"));
    }
    let map = legend_map(input, l, frame)?;
    let mut out: Vec<(String, Option<String>)> = Vec::new();
    for (n, label) in given {
        let label = match label {
            Value::Null => None,
            Value::String(t) if t.trim().is_empty() => None,
            // Refused rather than cut, so a label is never shown other than as given.
            Value::String(t) if t.chars().count() > MAX_LEGEND_LABEL_CHARS => {
                return Err(format!("a legend's label is at most {MAX_LEGEND_LABEL_CHARS} characters ('{n}' has {})", t.chars().count()));
            }
            Value::String(t) => Some(t.clone()),
            _ => return Err(LABELS_SHAPE.into()),
        };
        let name = legend_layer(map, n)?;
        // Which of two spellings of a name counts is not the caller's to guess: refused.
        if out.iter().any(|(o, _)| o.eq_ignore_ascii_case(name)) {
            return Err(format!("labels names layer '{name}' twice"));
        }
        out.push((name.clone(), label));
    }
    Ok(Some(out))
}

/// A legend's `labels` with those `given` ([`legend_labels`]) put in: each layer's label replaced,
/// or dropped for `None`; none at all for an empty list. More than a legend keeps is an error.
pub(in crate::cmd::layout) fn with_labels(
    labels: &BTreeMap<String, String>,
    given: Vec<(String, Option<String>)>,
) -> Result<BTreeMap<String, String>, String> {
    let mut out = if given.is_empty() { BTreeMap::new() } else { labels.clone() };
    for (name, label) in given {
        out.retain(|n, _| !n.eq_ignore_ascii_case(&name));
        if let Some(t) = label {
            out.insert(name, t);
        }
    }
    if out.len() > MAX_LEGEND_LABELS {
        return Err(format!("a legend can label at most {MAX_LEGEND_LABELS} layers"));
    }
    Ok(out)
}

/// The refusal for `classLabels` on an element other than a legend.
pub(in crate::cmd::layout) const ONLY_LEGEND_CLASS_LABELS: &str = "only legends take classLabels";

/// The shape a legend's `classLabels` takes.
pub(in crate::cmd::layout) const CLASS_LABELS_SHAPE: &str =
    r#"classLabels is an object of layer names and their classes' labels: {"<layer>": {"<class label>": "<text>" | null} | null}"#;

/// The most class names a refusal lists.
const CLASSES_SAID: usize = 10;

/// A legend's `classLabels` as given: each layer, as the map spells it, with each class and the
/// text to show for it (`None`: the layer's own label again), or `None` for the layer to show all
/// of its own labels again.
pub(in crate::cmd::layout) type GivenClassLabels = Vec<(String, Option<Vec<(String, Option<String>)>>)>;

/// A legend's `classLabels` from `p` (#3187): `None` when not given; an empty list for `{}`, which
/// shows every class's own label again. A text for a class the layer doesn't list or for a layer
/// with no classes to rename, a layer the map hasn't or a layer named twice is an error; taking a
/// text off isn't, whatever the layer lists now.
pub(in crate::cmd::layout) fn legend_class_labels(
    input: &impl crate::cmd::InputSource,
    l: &Layout,
    frame: u32,
    p: &Value,
) -> Result<Option<GivenClassLabels>, String> {
    let Some(v) = p.get("classLabels").filter(|v| !v.is_null()) else { return Ok(None) };
    let given = v.as_object().ok_or(CLASS_LABELS_SHAPE)?;
    let too_many = || format!("a legend can label at most {MAX_LEGEND_LABELS} classes");
    if given.len() > MAX_LEGEND_LABELS {
        return Err(format!("classLabels names at most {MAX_LEGEND_LABELS} layers"));
    }
    let map = legend_map(input, l, frame)?;
    let mut room = MAX_LEGEND_LABELS;
    let mut out: GivenClassLabels = Vec::new();
    for (n, classes) in given {
        let classes = match classes {
            Value::Null => None,
            Value::Object(c) if c.is_empty() => None,
            Value::Object(c) => Some(c),
            _ => return Err(CLASS_LABELS_SHAPE.into()),
        };
        let name = legend_layer(map, n)?;
        if out.iter().any(|(o, _)| o.eq_ignore_ascii_case(name)) {
            return Err(format!("classLabels names layer '{name}' twice"));
        }
        let Some(classes) = classes else {
            out.push((name.clone(), None));
            continue;
        };
        room = room.checked_sub(classes.len()).ok_or_else(too_many)?;
        // Read once a layer, and only if a text is given: layers may share a name.
        let mut listed: Option<Vec<String>> = None;
        let mut entries = Vec::new();
        for (class, text) in classes {
            let text = match text {
                Value::Null => None,
                Value::String(t) if t.trim().is_empty() => None,
                Value::String(t) if t.chars().count() > MAX_LEGEND_LABEL_CHARS => {
                    return Err(format!(
                        "a legend's label is at most {MAX_LEGEND_LABEL_CHARS} characters ('{class}' of '{name}' has {})",
                        t.chars().count()
                    ));
                }
                Value::String(t) => Some(t.clone()),
                _ => return Err(CLASS_LABELS_SHAPE.into()),
            };
            if text.is_some() {
                let listed = listed.get_or_insert_with(|| {
                    map.layers
                        .iter()
                        .filter(|l| l.name.eq_ignore_ascii_case(name))
                        .flat_map(crate::render::layout::class_row_labels)
                        .collect()
                });
                if listed.is_empty() {
                    return Err(format!("'{name}' has no classes to rename; give its name in labels"));
                }
                if !listed.contains(class) {
                    let more = if listed.len() > CLASSES_SAID { ", …" } else { "" };
                    let some = listed.iter().take(CLASSES_SAID).cloned().collect::<Vec<_>>().join(", ");
                    return Err(format!("'{name}' lists no class '{class}' (it lists: {some}{more})"));
                }
            }
            entries.push((class.clone(), text));
        }
        out.push((name.clone(), Some(entries)));
    }
    Ok(Some(out))
}

/// A legend's class labels with those `given` ([`legend_class_labels`]) put in, class by class;
/// none at all for an empty list. More than a legend keeps is an error.
pub(in crate::cmd::layout) fn with_class_labels(kept: &ClassLabels, given: GivenClassLabels) -> Result<ClassLabels, String> {
    let mut out = if given.is_empty() { ClassLabels::default() } else { kept.clone() };
    for (name, classes) in given {
        let spellings: Vec<String> = out.by_layer.keys().filter(|n| n.eq_ignore_ascii_case(&name)).cloned().collect();
        let mut own: BTreeMap<String, String> = spellings.iter().filter_map(|n| out.by_layer.remove(n)).flatten().collect();
        let Some(classes) = classes else { continue };
        for (class, text) in classes {
            match text {
                Some(text) => own.insert(class, text),
                None => own.remove(&class),
            };
        }
        if !own.is_empty() {
            out.by_layer.insert(name, own);
        }
    }
    if out.by_layer.values().map(BTreeMap::len).sum::<usize>() > MAX_LEGEND_LABELS {
        return Err(format!("a legend can label at most {MAX_LEGEND_LABELS} classes"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use rustgis_map::layout::LegendFit;
    use rustgis_render::layout::UnfittedLegend;

    use crate::{Project, Session};

    /// What [`super::say_unfitted_legends`] says of `listed`.
    fn said(listed: &[UnfittedLegend]) -> Vec<String> {
        let mut s = Session::new(Project::default());
        super::say_unfitted_legends(&mut s, listed);
        s.notices.iter().map(|n| n.text.clone()).collect()
    }

    /// A one-column legend listed twice, too wide on one page of a series and too tall on another,
    /// is under one reason, whose words are the same for both: named and counted once there. Two
    /// legends under one reason are still both, counted.
    #[test]
    fn a_legend_is_named_once_under_each_reason() {
        let cut = |id: u32, name: &str, (too_wide, too_tall): (bool, bool)| UnfittedLegend {
            id,
            name: name.into(),
            fit: LegendFit::AdjustSize,
            columns: 1,
            too_wide,
            too_tall,
        };
        let reason = "the legend lists more than fits its box even with its text shrunk to 4 points: its last rows or the ends of long labels are left out";
        assert_eq!(said(&[cut(7, "Roads", (true, false)), cut(7, "Roads", (false, true))]), [format!("Legend \"Roads\" (7): {reason}")]);
        assert_eq!(
            said(&[cut(7, "Roads", (true, false)), cut(8, "Rivers", (false, true)), cut(7, "Roads", (false, true))]),
            [format!("2 legends (\"Roads\" (7), \"Rivers\" (8)): {reason}")]
        );
    }
}
