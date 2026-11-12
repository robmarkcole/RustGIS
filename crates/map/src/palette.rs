//! Colour ramp files: RustGIS's own `.rgisramp` JSON, QGIS style XML (a Style Manager export or
//! a `.qml` layer style), GDAL colour-relief text (and QGIS colour map exports) and GMT `.cpt`
//! palettes, all read into [`CustomRamp`]s.
//!
//! The other formats tie colours to data values; a ramp keeps their relative spacing, from the
//! lowest value (0) to the highest (1).

use roxmltree::{Node, ParsingOptions};
use serde_json::Value;

use crate::color::{CustomRamp, MAX_RAMP_STOPS, Ramp, RampKind, Rgba};

/// Most ramps one file adds.
pub const MAX_RAMPS: usize = 1000;
/// Most lines or XML nodes read from a file.
const MAX_LINES: usize = 100_000;

/// A colour ramp file's format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RampFormat {
    /// `.rgisramp`: JSON, a list of ramps or one ramp.
    Json,
    /// QGIS style XML (`<qgis_style>`) or a `.qml` layer style.
    Qgis,
    /// GDAL `gdaldem color-relief` text, or a QGIS colour map export.
    Gdal,
    /// A GMT colour palette table.
    Cpt,
}

impl RampFormat {
    /// The format named `s` (`rgisramp`, `qgis`, `gdal` or `cpt`).
    pub fn from_name(s: &str) -> Option<RampFormat> {
        match s.trim().to_ascii_lowercase().as_str() {
            "rgisramp" | "json" => Some(RampFormat::Json),
            "qgis" | "xml" | "qml" => Some(RampFormat::Qgis),
            "gdal" | "txt" | "clr" => Some(RampFormat::Gdal),
            "cpt" | "gmt" => Some(RampFormat::Cpt),
            _ => None,
        }
    }

    /// The format of a file, from its name and contents.
    pub fn detect(file: Option<&str>, text: &str) -> RampFormat {
        let ext = file.and_then(|f| f.rsplit_once('.')).map(|(_, e)| e.to_ascii_lowercase());
        match ext.as_deref() {
            Some("rgisramp" | "json") => return RampFormat::Json,
            Some("xml" | "qml") => return RampFormat::Qgis,
            Some("cpt") => return RampFormat::Cpt,
            _ => {}
        }
        let t = text.trim_start_matches('\u{feff}').trim_start();
        if t.starts_with('<') {
            RampFormat::Qgis
        } else if t.starts_with('[') || t.starts_with('{') {
            RampFormat::Json
        } else if ext.as_deref() != Some("txt") && ext.as_deref() != Some("clr") && looks_like_cpt(t) {
            RampFormat::Cpt
        } else {
            RampFormat::Gdal
        }
    }
}

/// Reads the colour ramps in `text`. `file` is the file's name, which picks the format (unless
/// `format` does) and names a ramp from a format without names (unless `name` does). `kind`
/// overrides each ramp's kind.
pub fn read_ramps(
    text: &str,
    file: Option<&str>,
    format: Option<RampFormat>,
    name: Option<&str>,
    kind: Option<RampKind>,
) -> Result<Vec<CustomRamp>, String> {
    let text = text.trim_start_matches('\u{feff}');
    let stem = file
        .map(|f| f.rsplit(['/', '\\']).next().unwrap_or(f))
        .map(|f| f.rsplit_once('.').map_or(f, |(s, _)| s))
        .filter(|s| !s.trim().is_empty());
    let fallback = name.filter(|n| !n.trim().is_empty()).or(stem).unwrap_or("Imported ramp");
    let format = format.unwrap_or_else(|| RampFormat::detect(file, text));
    let mut ramps = match format {
        RampFormat::Json => read_json(text)?,
        RampFormat::Qgis => read_qgis(text, fallback)?,
        RampFormat::Gdal => vec![ramp(fallback, RampKind::Sequential, read_gdal(text)?)?],
        RampFormat::Cpt => vec![ramp(fallback, RampKind::Sequential, read_cpt(text)?)?],
    };
    if ramps.is_empty() {
        return Err("the file has no colour ramps".into());
    }
    if ramps.len() > MAX_RAMPS {
        return Err(format!("a colour ramp file holds at most {MAX_RAMPS} ramps"));
    }
    if let (Some(n), [r]) = (name.filter(|n| !n.trim().is_empty()), ramps.as_mut_slice()) {
        *r.name_mut() = n.trim().chars().take(200).collect();
    }
    // Other tools' ramps may share a built-in scheme's name (QGIS has "Greys"): keep both.
    if format != RampFormat::Json {
        for r in &mut ramps {
            if Ramp::from_name(r.name()).is_some() {
                r.name_mut().push_str(" (custom)");
            }
        }
    }
    if let Some(k) = kind {
        ramps = ramps
            .into_iter()
            .map(|r| CustomRamp::new(r.name(), k, r.stops().iter().map(|&(p, c)| (Some(p), c)).collect()))
            .collect::<Result<_, _>>()?;
    }
    Ok(ramps)
}

fn read_json(text: &str) -> Result<Vec<CustomRamp>, String> {
    let bad = |e: serde_json::Error| format!("not a RustGIS colour ramp file: {e}");
    match serde_json::from_str::<Value>(text).map_err(bad)? {
        Value::Array(a) if a.len() > MAX_RAMPS => Err(format!("a colour ramp file holds at most {MAX_RAMPS} ramps")),
        v @ Value::Array(_) => serde_json::from_value(v).map_err(bad),
        v => serde_json::from_value(v).map(|r| vec![r]).map_err(bad),
    }
}

/// A ramp from stops at any positions: they are scaled to 0..=1 (a stable sort keeps hard
/// edges in order) and, past [`MAX_RAMP_STOPS`], resampled evenly.
fn ramp(name: &str, kind: RampKind, mut stops: Vec<(f64, Rgba)>) -> Result<CustomRamp, String> {
    stops.retain(|s| s.0.is_finite());
    stops.sort_by(|a, b| a.0.total_cmp(&b.0));
    stops.dedup();
    let (Some(lo), Some(hi)) = (stops.first().map(|s| s.0), stops.last().map(|s| s.0)) else {
        return Err(format!("{name}: the file has no colours"));
    };
    let span = hi - lo;
    let mut stops: Vec<(f64, Rgba)> =
        stops.into_iter().map(|(v, c)| (if span > 0.0 { ((v - lo) / span).clamp(0.0, 1.0) } else { 0.0 }, c)).collect();
    if let ([(_, c)], false) = (stops.as_slice(), kind == RampKind::Qualitative) {
        // One colour: a flat ramp.
        stops = vec![(0.0, *c), (1.0, *c)];
    } else if span <= 0.0 {
        let n = stops.len().max(2) - 1;
        stops = stops.into_iter().enumerate().map(|(i, (_, c))| (i as f64 / n as f64, c)).collect();
    }
    if stops.len() > MAX_RAMP_STOPS {
        let n = MAX_RAMP_STOPS - 1;
        stops = (0..=n)
            .map(|i| {
                let t = i as f64 / n as f64;
                (t, sample(&stops, t))
            })
            .collect();
    }
    CustomRamp::new(name, kind, stops.into_iter().map(|(p, c)| (Some(p), c)).collect())
}

/// The colour at `t` of stops sorted by position.
fn sample(stops: &[(f64, Rgba)], t: f64) -> Rgba {
    let Some(&(_, mut c)) = stops.first() else { return Rgba::rgb(128, 128, 128) };
    for w in stops.windows(2) {
        if let [(p0, c0), (p1, c1)] = w {
            if t < *p0 {
                break;
            }
            c = if t <= *p1 && p1 > p0 { c0.lerp(*c1, (t - p0) / (p1 - p0)) } else { *c1 };
        }
    }
    c
}

// --- QGIS -------------------------------------------------------------------------------

fn read_qgis(text: &str, fallback: &str) -> Result<Vec<CustomRamp>, String> {
    // QGIS writes a bare `<!DOCTYPE qgis_style>`; drop it rather than allow DTDs.
    let text = strip_doctype(text);
    let opts = ParsingOptions { allow_dtd: false, nodes_limit: MAX_LINES as u32, ..ParsingOptions::default() };
    let doc = rustgis_data::xml::parse(&text, opts).map_err(|e| format!("not a QGIS style file: {e}"))?;
    let mut ramps = Vec::new();
    let mut skipped = Vec::new();
    // A style file's ramps are in <colorramps>; its symbols may hold ramps of their own.
    let library = doc.descendants().find(|n| n.has_tag_name("colorramps"));
    let nodes: Vec<Node> = match library {
        Some(l) => l.children().filter(|n| n.has_tag_name("colorramp")).collect(),
        None => doc.descendants().filter(|n| n.has_tag_name("colorramp")).collect(),
    };
    for node in nodes {
        if ramps.len() >= MAX_RAMPS {
            break;
        }
        let name = node.attribute("name").map(str::trim).filter(|n| !n.is_empty() && !n.starts_with('[')).unwrap_or(fallback);
        match qgis_ramp(node, name) {
            Ok(Some(r)) => ramps.push(r),
            Ok(None) => skipped.push(format!("{name} ({})", node.attribute("type").unwrap_or("?"))),
            Err(e) => return Err(e),
        }
    }
    // A raster layer style without a ramp element: its colour map.
    if ramps.is_empty()
        && let Some(shader) = doc.descendants().find(|n| n.has_tag_name("colorrampshader"))
    {
        let stops: Vec<(f64, Rgba)> = shader
            .children()
            .filter(|n| n.has_tag_name("item"))
            .filter_map(|n| Some((n.attribute("value")?.trim().parse().ok()?, qgis_color(n.attribute("color")?)?)))
            .collect();
        let discrete = shader.attribute("colorRampType") == Some("DISCRETE");
        let stops = if discrete { stepped_values(stops) } else { stops };
        ramps.push(ramp(fallback, RampKind::Sequential, stops)?);
    }
    if ramps.is_empty() && !skipped.is_empty() {
        return Err(format!("RustGIS reads QGIS gradient and preset colour ramps, not {}", skipped.join(", ")));
    }
    Ok(ramps)
}

/// The text without a `<!DOCTYPE …>` that has no internal subset.
fn strip_doctype(text: &str) -> String {
    let Some(i) = text.find("<!DOCTYPE") else { return text.to_string() };
    let rest = text.get(i..).unwrap_or("");
    match rest.find('>') {
        Some(j) if !rest.get(..j).unwrap_or("").contains('[') => {
            format!("{}{}", text.get(..i).unwrap_or(""), rest.get(j + 1..).unwrap_or(""))
        }
        _ => text.to_string(),
    }
}

/// A QGIS `<colorramp>`, or `None` for a kind we don't read (ColorBrewer, cpt-city, random).
fn qgis_ramp(node: Node, name: &str) -> Result<Option<CustomRamp>, String> {
    let props = qgis_props(node);
    let prop = |k: &str| props.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
    let bad = |what: &str, v: &str| format!("{name}: '{v}' is not a QGIS {what}");
    match node.attribute("type").unwrap_or("gradient") {
        "gradient" => {
            let c1 = prop("color1").unwrap_or("0,0,255,255");
            let c2 = prop("color2").unwrap_or("0,255,0,255");
            let c1 = qgis_color(c1).ok_or_else(|| bad("colour", c1))?;
            let c2 = qgis_color(c2).ok_or_else(|| bad("colour", c2))?;
            let mut stops = vec![(0.0, c1)];
            for s in qgis_stops(prop("stops").unwrap_or("")) {
                let (p, c) = s.split_once(';').unwrap_or((s, ""));
                let p: f64 = p.trim().parse().map_err(|_| bad("ramp stop", s))?;
                let c = c.split(';').next().and_then(qgis_color).ok_or_else(|| bad("ramp stop", s))?;
                stops.push((p.clamp(0.0, 1.0), c));
            }
            stops.push((1.0, c2));
            stops.sort_by(|a, b| a.0.total_cmp(&b.0));
            // A discrete ramp holds each colour up to the next stop.
            let discrete = prop("discrete").is_some_and(|d| d.trim() == "1");
            let stops = if discrete { stepped(stops) } else { stops };
            ramp(name, RampKind::Sequential, stops).map(Some)
        }
        "preset" => {
            let mut colors = Vec::new();
            for i in 0..MAX_RAMP_STOPS {
                let Some(c) = prop(&format!("preset_color_{i}")) else { break };
                colors.push(qgis_color(c).ok_or_else(|| bad("colour", c))?);
            }
            let n = colors.len().max(2) - 1;
            let stops = colors.into_iter().enumerate().map(|(i, c)| (i as f64 / n as f64, c)).collect();
            ramp(name, RampKind::Qualitative, stops).map(Some)
        }
        _ => Ok(None),
    }
}

/// A ramp's properties: `<prop k v>` children (older QGIS) or `<Option name value>` inside an
/// `<Option type="Map">` (QGIS 3.26 and later).
fn qgis_props(node: Node) -> Vec<(String, String)> {
    crate::qgis::props(node)
}

/// The stops of a QGIS gradient, `pos;color[;spec;direction]` joined by `:`. Newer colours
/// carry a `rgb:…` part with a colon of its own, so a piece that doesn't start with a position
/// belongs to the stop before it.
fn qgis_stops(s: &str) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    let mut start = 0;
    for (i, _) in s.match_indices(':') {
        let next = s.get(i + 1..).unwrap_or("");
        let pos = next.split(';').next().unwrap_or("");
        if next.contains(';') && pos.trim().parse::<f64>().is_ok() {
            out.push(s.get(start..i).unwrap_or(""));
            start = i + 1;
        }
    }
    out.push(s.get(start..).unwrap_or(""));
    out.into_iter().filter(|p| !p.trim().is_empty()).collect()
}

/// A QGIS colour: `r,g,b,a` (with anything after, such as `,rgb:…`) or `#RRGGBB[AA]`.
fn qgis_color(s: &str) -> Option<Rgba> {
    crate::qgis::color(s)
}

/// Stops at 0..=1 made into steps: each colour holds until the next stop's position.
fn stepped(stops: Vec<(f64, Rgba)>) -> Vec<(f64, Rgba)> {
    let mut out = Vec::with_capacity(stops.len() * 2);
    let n = stops.len();
    for (i, (p, c)) in stops.iter().enumerate() {
        // QGIS gives the last colour (at 1) only to the very top value: skip it.
        if i + 1 == n && i > 0 {
            break;
        }
        let end = stops.get(i + 1).map_or(1.0, |s| s.0);
        out.push((*p, *c));
        out.push((end, *c));
    }
    out
}

/// Value-coloured classes (QGIS discrete colour maps): each colour holds up to its value.
fn stepped_values(mut stops: Vec<(f64, Rgba)>) -> Vec<(f64, Rgba)> {
    stops.retain(|s| s.0.is_finite());
    stops.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out = Vec::with_capacity(stops.len() * 2);
    let mut prev = stops.first().map(|s| s.0);
    for (v, c) in stops {
        if let Some(p) = prev {
            out.push((p, c));
        }
        out.push((v, c));
        prev = Some(v);
    }
    out
}

// --- GDAL colour-relief text --------------------------------------------------------------

/// `value R G B [A]` lines (or a colour name instead of numbers), separated by spaces, tabs,
/// commas or colons; values may be percentages; `nv` (no data) lines are skipped. QGIS colour
/// map exports (`value,R,G,B,A,label`) read the same way.
fn read_gdal(text: &str) -> Result<Vec<(f64, Rgba)>, String> {
    let mut stops = Vec::new();
    let mut percent = None;
    for (n, line) in text.lines().take(MAX_LINES).enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.to_ascii_uppercase().starts_with("INTERPOLATION") {
            continue;
        }
        let t: Vec<&str> = line.split([' ', '\t', ',', ':']).filter(|t| !t.is_empty()).collect();
        let Some(&first) = t.first() else { continue };
        if first.eq_ignore_ascii_case("nv") {
            continue;
        }
        let bad = || format!("line {}: '{line}' is not a value and a colour", n + 1);
        let (v, pct) = match first.strip_suffix('%') {
            Some(p) => (p.parse::<f64>().map_err(|_| bad())? / 100.0, true),
            None => (first.parse::<f64>().map_err(|_| bad())?, false),
        };
        if *percent.get_or_insert(pct) != pct {
            return Err("the file mixes percentages and values".into());
        }
        let nums: Vec<u8> =
            t.iter().skip(1).take(4).map_while(|x| x.parse::<f64>().ok()).map(|x| x.round().clamp(0.0, 255.0) as u8).collect();
        let c = match nums.as_slice() {
            [r, g, b] => Rgba::rgb(*r, *g, *b),
            [r, g, b, a] => Rgba::new(*r, *g, *b, *a),
            _ => t.get(1).and_then(|s| named(s)).ok_or_else(bad)?,
        };
        stops.push((v, c));
    }
    if percent == Some(true) {
        // Percentages are positions already: keep them, with the ends at 0 and 100 %.
        stops.push((0.0, stops.iter().min_by(|a, b| a.0.total_cmp(&b.0)).map_or(Rgba::TRANSPARENT, |s| s.1)));
        stops.push((1.0, stops.iter().max_by(|a, b| a.0.total_cmp(&b.0)).map_or(Rgba::TRANSPARENT, |s| s.1)));
    }
    Ok(stops)
}

/// The colour names GDAL knows (its whole list), and the X11 grey levels GMT palettes use
/// (`gray0` to `gray100`).
fn named(s: &str) -> Option<Rgba> {
    let s = s.to_ascii_lowercase();
    if let Some(level) = s.strip_prefix("gray").or_else(|| s.strip_prefix("grey")).filter(|l| !l.is_empty())
        && let Ok(l) = level.parse::<u8>()
        && l <= 100
    {
        let g = (f64::from(l) * 2.55).round() as u8;
        return Some(Rgba::rgb(g, g, g));
    }
    Some(match s.as_str() {
        "white" => Rgba::rgb(255, 255, 255),
        "black" => Rgba::rgb(0, 0, 0),
        "red" => Rgba::rgb(255, 0, 0),
        "green" => Rgba::rgb(0, 255, 0),
        "blue" => Rgba::rgb(0, 0, 255),
        "yellow" => Rgba::rgb(255, 255, 0),
        "magenta" => Rgba::rgb(255, 0, 255),
        "cyan" | "aqua" => Rgba::rgb(0, 255, 255),
        "grey" | "gray" => Rgba::rgb(190, 190, 190),
        "orange" => Rgba::rgb(255, 165, 0),
        "brown" => Rgba::rgb(165, 42, 42),
        "purple" | "violet" => Rgba::rgb(238, 130, 238),
        "indigo" => Rgba::rgb(75, 0, 130),
        _ => return None,
    })
}

// --- GMT colour palette tables -------------------------------------------------------------

/// Whether text reads as a GMT palette: a `z color z color` line, or a `# COLOR_MODEL` comment.
fn looks_like_cpt(text: &str) -> bool {
    for line in text.lines().take(200) {
        let l = line.trim();
        if l.starts_with('#') {
            if l.to_ascii_uppercase().contains("COLOR_MODEL") {
                return true;
            }
            continue;
        }
        if l.is_empty() || matches!(l.chars().next(), Some('B' | 'F' | 'N')) {
            continue;
        }
        // All numbers in fewer than eight columns reads as GDAL's `value R G B`.
        let t: Vec<&str> = l.split(';').next().unwrap_or("").split_whitespace().collect();
        if t.len() < 8 && t.iter().all(|x| x.parse::<f64>().is_ok()) {
            return false;
        }
        return cpt_line(l, false).is_some();
    }
    false
}

/// `z0 color0 z1 color1` lines: a colour is `r/g/b`, `r g b`, `#RRGGBB`, a grey level, a name,
/// `h-s-v`, or `c/m/y/k`. Background, foreground and no-data (`B`, `F`, `N`) lines are skipped.
fn read_cpt(text: &str) -> Result<Vec<(f64, Rgba)>, String> {
    let mut hsv = false;
    let mut stops = Vec::new();
    for (n, line) in text.lines().take(MAX_LINES).enumerate() {
        let l = line.trim();
        if let Some(c) = l.strip_prefix('#') {
            let c = c.to_ascii_uppercase();
            if let Some((_, m)) = c.split_once("COLOR_MODEL") {
                hsv = m.contains("HSV");
                if m.contains("CMYK") {
                    return Err("RustGIS reads RGB and HSV GMT palettes, not CMYK ones".into());
                }
            }
            continue;
        }
        if l.is_empty() || matches!(l.chars().next(), Some('B' | 'F' | 'N')) {
            continue;
        }
        let [(z0, c0), (z1, c1)] = cpt_line(l, hsv).ok_or_else(|| format!("line {}: '{l}' is not a GMT palette line", n + 1))?;
        stops.push((z0, c0));
        stops.push((z1, c1));
    }
    Ok(stops)
}

fn cpt_line(line: &str, hsv: bool) -> Option<[(f64, Rgba); 2]> {
    // An annotation (`; label`) and the L/U/B flag after the colours don't matter.
    let line = line.split(';').next().unwrap_or("");
    let t: Vec<&str> = line.split_whitespace().collect();
    let width = if t.len() >= 8 && t.iter().take(8).all(|x| x.parse::<f64>().is_ok()) { 3 } else { 1 };
    let z0: f64 = t.first()?.parse().ok()?;
    let c0 = cpt_color(t.get(1..1 + width)?, hsv)?;
    let z1: f64 = t.get(1 + width)?.parse().ok()?;
    let c1 = cpt_color(t.get(2 + width..2 + 2 * width)?, hsv)?;
    Some([(z0, c0), (z1, c1)])
}

fn cpt_color(t: &[&str], hsv: bool) -> Option<Rgba> {
    let byte = |x: f64| x.round().clamp(0.0, 255.0) as u8;
    let nums = |parts: &[&str]| parts.iter().map(|p| p.trim().parse::<f64>().ok()).collect::<Option<Vec<f64>>>();
    match t {
        [_, _, _] => from_triplet(&nums(t)?, hsv),
        [c] if c.starts_with('#') => Rgba::from_hex(c),
        [c] if c.contains('/') => {
            let v = nums(&c.split('/').collect::<Vec<_>>())?;
            match v.as_slice() {
                [_, _, _] => from_triplet(&v, hsv),
                [c, m, y, k] => {
                    let ch = |x: f64| byte(255.0 * (1.0 - x / 100.0) * (1.0 - k / 100.0));
                    Some(Rgba::rgb(ch(*c), ch(*m), ch(*y)))
                }
                _ => None,
            }
        }
        // `h-s-v`: a hyphen after the first character.
        [c] if c.get(1..).is_some_and(|r| r.contains('-')) => {
            let (h, sv) = c.split_at(c.get(1..)?.find('-')? + 1);
            let v = nums(&[h, sv.trim_start_matches('-').split('-').next()?, sv.trim_start_matches('-').split('-').nth(1)?])?;
            from_triplet(&v, true)
        }
        [c] => match c.parse::<f64>() {
            Ok(g) => Some(Rgba::rgb(byte(g), byte(g), byte(g))),
            Err(_) => named(c.split('@').next().unwrap_or(c)),
        },
        _ => None,
    }
}

/// An `r g b` (0..255) or `h s v` (0..360, 0..1, 0..1) triplet.
fn from_triplet(v: &[f64], hsv: bool) -> Option<Rgba> {
    let [a, b, c] = v else { return None };
    let byte = |x: f64| (x * 255.0).round().clamp(0.0, 255.0) as u8;
    if !hsv {
        return Some(Rgba::rgb(byte(a / 255.0), byte(b / 255.0), byte(c / 255.0)));
    }
    let (h, s, v) = (a.rem_euclid(360.0) / 60.0, s_clamp(*b), s_clamp(*c));
    let x = v * s;
    let m = v - x;
    let f = h - h.floor();
    let (r, g, b) = match h.floor() as u8 {
        0 => (v, m + x * f, m),
        1 => (m + x * (1.0 - f), v, m),
        2 => (m, v, m + x * f),
        3 => (m, m + x * (1.0 - f), v),
        4 => (m + x * f, m, v),
        _ => (v, m, m + x * (1.0 - f)),
    };
    Some(Rgba::rgb(byte(r), byte(g), byte(b)))
}

fn s_clamp(x: f64) -> f64 {
    if x.is_finite() { x.clamp(0.0, 1.0) } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hexes(r: &CustomRamp) -> Vec<String> {
        r.stops().iter().map(|s| s.1.hex()).collect()
    }

    fn read(text: &str, file: Option<&str>) -> Result<Vec<CustomRamp>, String> {
        read_ramps(text, file, None, None, None)
    }

    /// A QGIS style file nested past the limit is refused before it is parsed, never a crash.
    #[test]
    fn a_deep_qgis_style_is_refused() {
        let deep = format!("<!DOCTYPE qgis_style><qgis_style>{}{}</qgis_style>", "<a>".repeat(10_000), "</a>".repeat(10_000));
        let refused = read(&deep, Some("deep.xml")).unwrap_err();
        assert!(refused.contains("deep"), "{refused}");
    }

    #[test]
    fn phosphor_qgis_and_gdal_exports() {
        // As Phosphor writes them.
        let xml = r#"<!DOCTYPE qgis_style>
<qgis_style version="2">
  <symbols/>
  <colorramps>
    <colorramp type="gradient" name="Phosphor Frostfire" tags="Phosphor">
      <Option type="Map">
        <Option type="QString" name="color1" value="239,246,255,255"/>
        <Option type="QString" name="color2" value="255,245,218,255"/>
        <Option type="QString" name="discrete" value="0"/>
        <Option type="QString" name="rampType" value="gradient"/>
        <Option type="QString" name="stops" value="0.25;123,92,166,255:0.5;202,116,130,255"/>
      </Option>
    </colorramp>
  </colorramps>
</qgis_style>"#;
        let r = read(xml, Some("Phosphor_Frostfire.xml")).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].name(), "Phosphor Frostfire");
        assert_eq!(hexes(&r[0]), ["#EFF6FF", "#7B5CA6", "#CA7482", "#FFF5DA"]);
        assert_eq!(r[0].stops()[1].0, 0.25);

        let txt = "100, 239, 246, 255, 255\n200, 123, 92, 166, 255\n500, 255, 245, 218, 255\nnv, 239, 246, 255, 255\n";
        let r = read(txt, Some("dir/Phosphor_Frostfire.txt")).unwrap();
        assert_eq!(r[0].name(), "Phosphor_Frostfire");
        assert_eq!(hexes(&r[0]), ["#EFF6FF", "#7B5CA6", "#FFF5DA"]);
        assert_eq!(r[0].stops()[1].0, 0.25, "values keep their spacing");
        let r = read("0 red\n1 blue\n", Some("greys.txt")).unwrap();
        assert_eq!(r[0].name(), "greys (custom)", "a built-in scheme's name is kept apart");
        // A name and kind given with the file.
        let r = read_ramps(txt, None, None, Some("Frost"), Some(RampKind::Diverging)).unwrap();
        assert_eq!((r[0].name(), r[0].kind()), ("Frost", RampKind::Diverging));
    }

    #[test]
    fn qgis_variants() {
        // Older <prop> form, a newer colour with its rgb: part, a discrete ramp, a preset
        // ramp and one we don't read.
        let xml = r#"<qgis_style version="2"><colorramps>
  <colorramp type="gradient" name="Old">
    <prop k="color1" v="0,0,0,255"/><prop k="color2" v="255,255,255,255"/>
    <prop k="stops" v="0.5;255,0,0,255,rgb:1,0,0,1;rgb;ccw"/>
  </colorramp>
  <colorramp type="gradient" name="Steps">
    <prop k="color1" v="0,0,0,255"/><prop k="color2" v="255,255,255,255"/><prop k="discrete" v="1"/>
    <prop k="stops" v="0.5;255,0,0,255"/>
  </colorramp>
  <colorramp type="preset" name="Cats">
    <prop k="preset_color_0" v="255,0,0,255"/><prop k="preset_color_1" v="0,255,0,255"/><prop k="preset_color_2" v="0,0,255,255"/>
  </colorramp>
  <colorramp type="colorbrewer" name="Brewer"><prop k="schemeName" v="Blues"/></colorramp>
</colorramps></qgis_style>"#;
        let r = read(xml, None).unwrap();
        assert_eq!(r.iter().map(CustomRamp::name).collect::<Vec<_>>(), ["Old", "Steps", "Cats"]);
        assert_eq!(hexes(&r[0]), ["#000000", "#FF0000", "#FFFFFF"]);
        assert_eq!(
            r[1].stops(),
            [(0.0, Rgba::rgb(0, 0, 0)), (0.5, Rgba::rgb(0, 0, 0)), (0.5, Rgba::rgb(255, 0, 0)), (1.0, Rgba::rgb(255, 0, 0))]
        );
        assert_eq!(r[2].kind(), RampKind::Qualitative);
        assert_eq!(hexes(&r[2]), ["#FF0000", "#00FF00", "#0000FF"]);
        let only = r#"<qgis_style><colorramps><colorramp type="cpt-city" name="X"/></colorramps></qgis_style>"#;
        assert!(read(only, None).unwrap_err().contains("X (cpt-city)"));

        // A raster layer style's colour map.
        let qml = r##"<qgis><pipe><rasterrenderer type="singlebandpseudocolor"><rastershader>
  <colorrampshader colorRampType="INTERPOLATED">
    <colorramp type="gradient" name="[source]"><Option type="Map">
      <Option name="color1" value="0,0,0,255" type="QString"/><Option name="color2" value="255,255,255,255" type="QString"/>
    </Option></colorramp>
    <item value="0" color="#000000" alpha="255" label="0"/><item value="10" color="#ffffff" alpha="255" label="10"/>
  </colorrampshader></rastershader></rasterrenderer></pipe></qgis>"##;
        let r = read(qml, Some("dem.qml")).unwrap();
        assert_eq!(r[0].name(), "dem", "the [source] ramp takes the file's name");
        let items = qml.replace(
            r#"<colorramp type="gradient" name="[source]"><Option type="Map">
      <Option name="color1" value="0,0,0,255" type="QString"/><Option name="color2" value="255,255,255,255" type="QString"/>
    </Option></colorramp>"#,
            "",
        );
        let r = read(&items, Some("dem.qml")).unwrap();
        assert_eq!(hexes(&r[0]), ["#000000", "#FFFFFF"]);
        assert!(read("<qgis_style><colorramps>", None).is_err());
    }

    #[test]
    fn gdal_text() {
        // Spaces, tabs and colons, percentages, names, a QGIS colour map export with labels.
        let r = read("0% blue\n50%\t255:0:0\n100% white\n", None).unwrap();
        assert_eq!(hexes(&r[0]), ["#0000FF", "#FF0000", "#FFFFFF"]);
        assert_eq!(r[0].stops()[1].0, 0.5);
        let r = read("10% blue\n90% white\n", None).unwrap();
        assert_eq!(r[0].stops().len(), 4, "the ends are held to 0 and 100 %");
        let qgis = "# QGIS Generated Color Map Export File\nINTERPOLATION:INTERPOLATED\n10,0,0,0,255,10\n30,255,255,255,255,30\n";
        assert_eq!(hexes(&read(qgis, Some("map.txt")).unwrap()[0]), ["#000000", "#FFFFFF"]);
        // Unsorted values are sorted.
        let r = read("5 255 255 255\n-5 0 0 0\n", None).unwrap();
        assert_eq!(hexes(&r[0]), ["#000000", "#FFFFFF"]);
        assert!(read("10% blue\n20 red\n", None).unwrap_err().contains("mixes"));
        assert!(read("hello world\n", Some("x.txt")).is_err());
        assert!(read("nv 0 0 0\n", Some("x.txt")).is_err(), "no colours");
        assert!(read("0 nothing\n", Some("x.txt")).is_err());
    }

    #[test]
    fn gmt_cpt() {
        let cpt = "# a palette\n# COLOR_MODEL = RGB\n-10 0/0/255 0 255/255/255\n0 255 255 255 10 255 0 0 ; hot\nB black\nF white\nN 128\n";
        assert_eq!(RampFormat::detect(None, cpt), RampFormat::Cpt);
        let r = read(cpt, Some("relief.cpt")).unwrap();
        assert_eq!(r[0].name(), "relief");
        assert_eq!(hexes(&r[0]), ["#0000FF", "#FFFFFF", "#FF0000"]);
        assert_eq!(r[0].stops()[1].0, 0.5);
        // Hard edges, hex, grey levels, names and HSV.
        let r = read("0 #000000 1 #000000\n1 255 2 white\n2 gray80 3 grey100\n", Some("a.cpt")).unwrap();
        assert_eq!(r[0].stops().len(), 6);
        assert_eq!(r[0].stops()[4].1, Rgba::rgb(204, 204, 204), "GMT's X11 grey levels, as in its globe palette");
        assert!(read("0 gray101 1 white\n", Some("a.cpt")).is_err());
        let r = read("# COLOR_MODEL = HSV\n0 0-1-1 1 120-1-1\n1 240 1 1 2 0 0 1\n", Some("h.cpt")).unwrap();
        assert_eq!(hexes(&r[0]), ["#FF0000", "#00FF00", "#0000FF", "#FFFFFF"]);
        let r = read("0 0/0/0/100 1 0/0/0/0\n", Some("k.cpt")).unwrap();
        assert_eq!(hexes(&r[0]), ["#000000", "#FFFFFF"]);
        assert!(read("# COLOR_MODEL = CMYK\n0 0/0/0/0 1 0/0/0/0\n", Some("k.cpt")).is_err());
        assert!(read("0 0/0/0\n", Some("k.cpt")).is_err());
        // Many segments are resampled down to the most stops a ramp keeps.
        let many: String = (0..400).map(|i| format!("{i} {} {} {}/0/0\n", i % 256, i + 1, (i + 1) % 256)).collect();
        assert_eq!(read(&many, Some("m.cpt")).unwrap()[0].stops().len(), MAX_RAMP_STOPS);
    }

    #[test]
    fn json_and_detection() {
        let one = r##"{"name": "A", "stops": ["#000000", "#ffffff"]}"##;
        assert_eq!(read(one, None).unwrap()[0].name(), "A");
        assert_eq!(read(&format!("[{one},{one}]"), None).unwrap().len(), 2);
        assert!(read("[]", None).is_err());
        assert!(read(&format!("[{}]", vec![one; MAX_RAMPS + 1].join(",")), None).is_err());
        assert_eq!(RampFormat::detect(Some("x.txt"), "0 0/0/0 1 0/0/0"), RampFormat::Gdal, "the extension wins");
        assert_eq!(RampFormat::detect(None, "0 0 0 0\n1 255 255 255"), RampFormat::Gdal);
        assert_eq!(RampFormat::detect(None, "0 0 0 0 1 255 255 255"), RampFormat::Cpt);
        assert_eq!(RampFormat::from_name("QGIS"), Some(RampFormat::Qgis));
        // A single colour makes a flat ramp; junk never panics.
        assert_eq!(read("0 red\n", Some("x.txt")).unwrap()[0].stops().len(), 2);
        for junk in ["", "\u{feff}", "<", "{", "0", "0 #", "0 1-", "0 -1-1-1 1 -", "1e400 red", "0 0/0/0/0/0 1 0/0/0"] {
            for f in [None, Some("x.cpt"), Some("x.txt"), Some("x.xml")] {
                let _ = read(junk, f);
            }
        }
    }
}
