//! Open-data portals: OpenStreetMap data for the map's area (Overpass) and a country's
//! administrative boundaries (geoBoundaries). Requests go off the frame through the tile cache's
//! downloads (a thread natively, fetch on the web); the answers are handed to `osm.download` and
//! `boundaries.add`, so the layers are added like any command's.

use egui::vec2;
use rustgis_engine::render::rustgis_tiles::{address::Reach, boundaries, cache, overpass};
use serde_json::{Value, json};

use crate::{RustGisApp, Slot};

/// An answer, with the reach it was read at.
type Answer = Slot<Result<(Vec<u8>, Reach), String>>;

/// What is being fetched, with the parameters its answer is added with.
enum Asked {
    Osm(Value),
    Countries,
    /// A country's level: its description, then its GeoJSON.
    Meta(Value),
    Boundary(Value),
}

/// The dialogs' choices and the request under way.
pub struct State {
    pub preset: String,
    pub tags: String,
    pub server: String,
    /// geoBoundaries' countries: code and name.
    pub countries: Vec<(String, String)>,
    /// The country list was asked for (once a session: an empty or failed answer leaves the code
    /// to be typed, with `countries_error` saying why).
    countries_asked: bool,
    pub countries_error: Option<String>,
    pub country: String,
    country_filter: String,
    /// Names as received, with their search text cached once per country list.
    country_search: std::collections::BTreeMap<String, (String, String)>,
    /// Each successful catalogue is authoritative only for its own level.
    availability: [Option<std::collections::BTreeMap<String, Option<u64>>>; 6],
    availability_errors: [Option<String>; 6],
    level_waiting: Vec<(usize, Answer)>,
    pub level: String,
    waiting: Option<(Asked, Answer)>,
    pub error: Option<String>,
    pub info: Option<String>,
}

impl Default for State {
    fn default() -> Self {
        State {
            preset: "buildings".into(),
            tags: String::new(),
            server: String::new(),
            countries: Vec::new(),
            countries_asked: false,
            countries_error: None,
            country: String::new(),
            country_filter: String::new(),
            country_search: Default::default(),
            availability: Default::default(),
            availability_errors: Default::default(),
            level_waiting: Vec::new(),
            level: "ADM1".into(),
            waiting: None,
            error: None,
            info: None,
        }
    }
}

/// Fetches `url` off the frame within `ceiling`: the reach of the answer that named it
/// (`Reach::Local` for a service the user or RustGIS names).
fn fetch(url: &str, ceiling: Reach) -> Answer {
    crate::fetch_within(url, cache::MAX_DOWNLOAD, None, ceiling)
}

/// Catalogues are small metadata lists, not feature downloads. Keep even an oversized service
/// answer bounded independently of the large data-download allowance.
const MAX_CATALOGUE: usize = 4 << 20;

fn fetch_catalogue(url: &str) -> Answer {
    crate::fetch_within(url, MAX_CATALOGUE, None, Reach::Local)
}

/// Lands the answers.
pub fn poll(app: &mut RustGisApp, ctx: &egui::Context) {
    app.ui.portals.level_waiting.retain(|(level, slot)| {
        let Some(result) = slot.lock().unwrap_or_else(|e| e.into_inner()).take() else {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            return true;
        };
        let result = result.and_then(|(bytes, _)| boundaries::parse_availability(&bytes));
        if let (Some(catalogue), Some(error)) =
            (app.ui.portals.availability.get_mut(*level), app.ui.portals.availability_errors.get_mut(*level))
        {
            match result {
                Ok(available) => {
                    *catalogue = Some(available);
                    *error = None;
                }
                Err(reason) => {
                    *catalogue = None;
                    *error = Some(reason);
                }
            }
        }
        false
    });
    let Some((_, slot)) = &app.ui.portals.waiting else { return };
    let Some(r) = slot.lock().unwrap_or_else(|e| e.into_inner()).take() else {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
        return;
    };
    let Some((asked, _)) = app.ui.portals.waiting.take() else { return };
    let reach = r.as_ref().map_or(Reach::Public, |(_, reach)| *reach);
    let r = r.map(|(bytes, _)| bytes);
    app.ui.portals.error = None;
    app.ui.portals.info = None;
    let text = |b: Vec<u8>| String::from_utf8(b).map_err(|_| "the answer isn't UTF-8 text".to_string());
    let done = match asked {
        Asked::Osm(mut p) => r.and_then(text).and_then(|t| {
            p["answer"] = json!(t);
            app.run("osm.download", p).map(|v| {
                let n = |k: &str| v["features"][k].as_u64().unwrap_or(0);
                app.ui.portals.info = Some(crate::i18n::fmt(
                    "Added {p} points, {l} lines and {g} polygons",
                    &[("p", &n("points").to_string()), ("l", &n("lines").to_string()), ("g", &n("polygons").to_string())],
                ));
            })
        }),
        // The list's own outcome, kept apart from the dialog's actions (a failure leaves the code to
        // be typed, and isn't asked again).
        Asked::Countries => {
            app.ui.portals.countries_error = match r.and_then(|b| boundaries::parse_countries(&b)) {
                Ok(c) if c.is_empty() => Some(tl!("geoBoundaries listed no countries: type a country's code").to_string()),
                Ok(c) => {
                    app.ui.portals.country_search =
                        c.iter().map(|(iso, name)| (iso.clone(), (name.clone(), format!("{name} {iso}").to_lowercase()))).collect();
                    app.ui.portals.countries = c;
                    None
                }
                Err(e) => Some(crate::i18n::fmt("The country list couldn't be read ({e}): type a country's code", &[("e", &e)])),
            };
            Ok(())
        }
        Asked::Meta(mut p) => r.and_then(|b| boundaries::parse_meta(&b)).map(|m| {
            p["name"] = json!(format!("{} {}", m.name, m.level));
            // The description names where the boundaries are: they are read no further than it was.
            app.ui.portals.waiting = Some((Asked::Boundary(p), fetch(&m.geojson, reach)));
        }),
        Asked::Boundary(mut p) => r.and_then(text).and_then(|t| {
            p["text"] = json!(t);
            app.run("boundaries.add", p).map(|_| ())
        }),
    };
    match done {
        Ok(()) => app.map.invalidate(),
        Err(e) => app.ui.portals.error = Some(e),
    }
}

fn status(app: &RustGisApp, ui: &mut egui::Ui) {
    if app.ui.portals.waiting.is_some() {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(tl!("Downloading…"));
        });
    }
    let t = crate::theme::Tokens::get(ui.ctx());
    if let Some(e) = &app.ui.portals.error {
        ui.label(egui::RichText::new(e).color(t.red));
    }
    if let Some(i) = &app.ui.portals.info {
        ui.label(egui::RichText::new(i).weak());
    }
}

/// OpenStreetMap: a preset or tag filters, downloaded for the map's extent.
pub fn osm(app: &mut RustGisApp, ui: &mut egui::Ui) {
    ui.label(egui::RichText::new(tl!("OpenStreetMap data where the map is, from an Overpass server: a preset, or tag filters such as amenity=cafe, shop. The area can be up to about 50 × 50 km. © OpenStreetMap contributors (ODbL).")).weak());
    let busy = app.ui.portals.waiting.is_some();
    egui::Grid::new("osm").num_columns(2).spacing(vec2(10.0, 8.0)).show(ui, |ui| {
        let st = &mut app.ui.portals;
        let input_label = ui.label(tl!("Preset"));
        let shown = overpass::PRESETS
            .iter()
            .find(|(id, _, _)| *id == st.preset)
            .map(|(_, l, _)| tl!(*l).to_string())
            .unwrap_or_else(|| tl!("None").to_string());
        egui::ComboBox::from_id_salt("osm_preset")
            .selected_text(shown)
            .width(260.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut st.preset, String::new(), tl!("None"));
                for (id, label, _) in overpass::PRESETS {
                    ui.selectable_value(&mut st.preset, (*id).to_string(), tl!(*label));
                }
            })
            .response
            .labelled_by(input_label.id);
        ui.end_row();
        let input_label = ui.label(tl!("Tags"));
        ui.add(egui::TextEdit::singleline(&mut st.tags).hint_text("amenity=cafe, shop").desired_width(260.0)).labelled_by(input_label.id);
        ui.end_row();
        let input_label = ui.label(tl!("Server"));
        ui.add(egui::TextEdit::singleline(&mut st.server).hint_text(overpass::OVERPASS).desired_width(260.0)).labelled_by(input_label.id);
        ui.end_row();
    });
    if ui.add_enabled(!busy, egui::Button::new(tl!("Download"))).clicked() {
        let st = &app.ui.portals;
        let mut p = json!({"preset": st.preset, "tags": st.tags});
        if !st.server.trim().is_empty() {
            p["url"] = json!(st.server.trim());
        }
        let url = rustgis_engine::cmd::portals::osm_query(&app.session, &p).and_then(|q| {
            // The box the request was for, so the answer is read for the same one.
            p["bbox"] = json!(q.bbox);
            q.url(Some(app.ui.portals.server.as_str()))
        });
        match url {
            Ok(url) => {
                app.ui.portals.error = None;
                app.ui.portals.waiting = Some((Asked::Osm(p), fetch(&url, Reach::Local)));
            }
            Err(e) => app.ui.portals.error = Some(e),
        }
    }
    status(app, ui);
}

/// A successfully read catalogue is authoritative for its level. Unknown catalogues and
/// invalid or unfinished country codes leave the choices open for normal input validation.
fn level_info(st: &State, level: &str) -> (bool, Option<u64>) {
    let Ok(country) = boundaries::country(&st.country) else {
        return (true, None);
    };
    let catalogue =
        level.strip_prefix("ADM").and_then(|n| n.parse::<usize>().ok()).and_then(|n| st.availability.get(n)).and_then(Option::as_ref);
    match catalogue {
        Some(c) => (c.contains_key(&country), c.get(&country).copied().flatten()),
        None => (true, None),
    }
}

/// Explains a disabled choice with the country's name when the country list is available.
fn missing_level(st: &State, level: &str) -> String {
    let country = boundaries::country(&st.country).unwrap_or_else(|_| st.country.clone());
    let name = st.countries.iter().find(|(iso, _)| *iso == country).map_or(country.as_str(), |(_, name)| name.as_str());
    crate::i18n::fmt("geoBoundaries has no {level} for {country}", &[("level", level), ("country", name)])
}

/// Boundaries: a country's administrative areas at a level, from geoBoundaries.
pub fn boundaries(app: &mut RustGisApp, ui: &mut egui::Ui) {
    ui.label(egui::RichText::new(tl!("A country's administrative boundaries from geoBoundaries (CC BY 4.0): the country itself (ADM0), its first-level divisions (ADM1) and finer ones.")).weak());
    let busy = app.ui.portals.waiting.is_some();
    // The country list, asked for once.
    if !app.ui.portals.countries_asked && !busy {
        app.ui.portals.countries_asked = true;
        for level in 1..=5 {
            if let Ok(url) = boundaries::level_url(&level.to_string()) {
                app.ui.portals.level_waiting.push((level, fetch_catalogue(&url)));
            }
        }
        app.ui.portals.waiting = Some((Asked::Countries, fetch_catalogue(&boundaries::countries_url())));
    }
    if let Some(e) = &app.ui.portals.countries_error {
        ui.label(egui::RichText::new(e).small().weak());
    }
    egui::Grid::new("boundaries").num_columns(2).spacing(vec2(10.0, 8.0)).show(ui, |ui| {
        let st = &mut app.ui.portals;
        let input_label = ui.label(tl!("Country"));
        if st.countries.is_empty() {
            ui.add(egui::TextEdit::singleline(&mut st.country).hint_text("CHE").desired_width(260.0)).labelled_by(input_label.id);
        } else {
            let selected = boundaries::country(&st.country).ok();
            let shown = st
                .countries
                .iter()
                .find(|(iso, _)| Some(iso.as_str()) == selected.as_deref())
                .map(|(iso, n)| format!("{n} ({iso})"))
                .unwrap_or_else(|| tl!("Choose a country").to_string());
            egui::ComboBox::from_id_salt("gb_country")
                .selected_text(shown)
                .width(260.0)
                .height(360.0)
                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                .show_ui(ui, |ui| {
                    ui.add(egui::TextEdit::singleline(&mut st.country_filter).hint_text(tl!("Search countries")))
                        .labelled_by(input_label.id);
                    let needle = st.country_filter.trim().to_lowercase();
                    for (iso, name) in &st.countries {
                        let cached = st.country_search.get(iso).filter(|(original, _)| original == name);
                        let matches = cached.map_or_else(
                            || format!("{name} {iso}").to_lowercase().contains(&needle),
                            |(_, search)| search.contains(&needle),
                        );
                        if !matches {
                            continue;
                        }
                        if ui.selectable_value(&mut st.country, iso.clone(), format!("{name} ({iso})")).clicked() {
                            ui.close();
                        }
                    }
                })
                .response
                .labelled_by(input_label.id);
        }
        ui.end_row();
        let input_label = ui.label(tl!("Level"));
        egui::ComboBox::from_id_salt("gb_level")
            .selected_text(st.level.clone())
            .width(260.0)
            .show_ui(ui, |ui| {
                for (l, label) in [
                    ("ADM0", "ADM0 — the country"),
                    ("ADM1", "ADM1 — first-level divisions"),
                    ("ADM2", "ADM2 — second-level divisions"),
                    ("ADM3", "ADM3 — third-level divisions"),
                    ("ADM4", "ADM4 — fourth-level divisions"),
                    ("ADM5", "ADM5 — fifth-level divisions"),
                ] {
                    let (available, count) = level_info(st, l);
                    let shown = count.map_or_else(
                        || tl!(label).to_string(),
                        |n| {
                            let key = if n == 1 { "{level} — {count} unit" } else { "{level} — {count} units" };
                            crate::i18n::fmt(key, &[("level", l), ("count", &n.to_string())])
                        },
                    );
                    let response = ui.add_enabled(available, egui::Button::selectable(st.level == l, shown));
                    if response.clicked() {
                        st.level = l.to_string();
                    }
                    if !available {
                        response.on_disabled_hover_text(missing_level(st, l));
                    }
                }
            })
            .response
            .labelled_by(input_label.id);
        ui.end_row();
    });
    let st = &app.ui.portals;
    let (available, _) = level_info(st, &st.level);
    if let Some(error) = st
        .level
        .strip_prefix("ADM")
        .and_then(|n| n.parse::<usize>().ok())
        .and_then(|n| st.availability_errors.get(n))
        .and_then(Option::as_ref)
    {
        ui.small(crate::i18n::fmt(
            "Availability for {level} couldn't be read ({error}); this level remains offered without unit counts.",
            &[("level", &st.level), ("error", error)],
        ));
    }
    if !available {
        ui.small(missing_level(st, &st.level));
    }
    if ui
        .add_enabled(!busy && available, egui::Button::new(tl!("Add")))
        .on_disabled_hover_text(if available { tl!("Downloading…").to_string() } else { missing_level(st, &st.level) })
        .clicked()
    {
        let st = &app.ui.portals;
        match boundaries::country(&st.country).and_then(|c| Ok((c, boundaries::level(&st.level)?))) {
            Ok((c, l)) => {
                let url = boundaries::meta_url(&c, &l);
                app.ui.portals.error = None;
                app.ui.portals.waiting = Some((Asked::Meta(json!({"country": c, "level": l})), fetch(&url, Reach::Local)));
            }
            Err(e) => app.ui.portals.error = Some(e),
        }
    }
    status(app, ui);
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_engine::render::rustgis_tiles::address::Origin;

    /// Exercises the actual Web Data pane: normalized typed codes, disabled levels and
    /// country filtering retain their accessible labels and affect the Add action.
    #[test]
    fn boundary_picker_filters_and_keeps_typed_codes_available() {
        use egui_kittest::kittest::{By, NodeT, Queryable};
        let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
        app.ui.portals.countries_asked = true;
        app.ui.portals.country = " per ".into();
        app.ui.portals.availability[1] = Some(std::collections::BTreeMap::from([("PER".into(), Some(26))]));
        app.ui.portals.availability[3] = Some(Default::default());
        app.run("ui.dialog", json!({"name":"boundaries"})).unwrap();
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1400.0, 900.0)).build_ui_state(
            |ui, app: &mut RustGisApp| {
                app.logic(ui.ctx());
                app.ui(ui);
            },
            app,
        );
        h.run();
        assert!(!h.get_by_label("Add").accesskit_node().is_disabled());
        h.get_by_label("Level").click();
        h.run();
        assert!(!h.get_by_label("ADM1 — 26 units").accesskit_node().is_disabled());
        assert!(h.get_by_label("ADM3 — third-level divisions").accesskit_node().is_disabled());
        h.get_by_label("ADM1 — 26 units").click();
        h.run();
        h.state_mut().ui.portals.countries = vec![("PER".into(), "Peru".into()), ("POL".into(), "Poland".into())];
        h.step();
        h.get_by_label("Country").click();
        h.run();
        let input = h.get(By::new().role(egui::accesskit::Role::TextInput).label("Country"));
        input.click();
        h.run();
        h.get(By::new().role(egui::accesskit::Role::TextInput).label("Country")).type_text("pol");
        h.run();
        assert!(h.query_by_label("Peru (PER)").is_none());
        h.get_by_label("Poland (POL)").click();
        h.run();
        assert_eq!(h.state().ui.portals.country, "POL");
        assert!(h.get_by_label("Add").accesskit_node().is_disabled());
    }

    /// Availability lands even with no dialog open and a failed level never disables choices.
    #[test]
    fn availability_lands_independently_of_boundary_downloads() {
        use egui_kittest::kittest::Queryable;
        let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
        let answer = |result| std::sync::Arc::new(std::sync::Mutex::new(Some(result)));
        app.ui.portals.level_waiting = vec![
            (2, answer(Ok((br#"[{"boundaryISO":"PER","admUnitCount":"196"}]"#.to_vec(), Reach::Public)))),
            (3, answer(Ok((b"[]".to_vec(), Reach::Public)))),
            (4, answer(Err("offline".to_string()))),
            (5, answer(Ok((b"{}".to_vec(), Reach::Public)))),
        ];
        poll(&mut app, &egui::Context::default());
        assert_eq!(app.ui.portals.availability[2].as_ref().unwrap().get("PER"), Some(&Some(196)));
        assert!(!app.ui.portals.availability[3].as_ref().unwrap().contains_key("PER"));
        assert!(app.ui.portals.availability[4].is_none());
        assert_eq!(app.ui.portals.availability_errors[4].as_deref(), Some("offline"));
        assert_eq!(app.ui.portals.availability_errors[5].as_deref(), Some("the geoBoundaries list isn't a list"));
        assert!(app.ui.portals.level_waiting.is_empty());
        app.ui.portals.countries_asked = true;
        app.ui.portals.country = "PER".into();
        app.run("ui.dialog", json!({"name":"boundaries"})).unwrap();
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1400.0, 900.0)).build_ui_state(
            |ui, app: &mut RustGisApp| {
                app.logic(ui.ctx());
                app.ui(ui);
            },
            app,
        );
        h.run();
        assert!(h.query_by_label_contains("couldn't be read").is_none());
        for (level, reason) in [("ADM4", "offline"), ("ADM5", "the geoBoundaries list isn't a list")] {
            h.state_mut().ui.portals.level = level.into();
            h.run();
            let note = crate::i18n::fmt(
                "Availability for {level} couldn't be read ({error}); this level remains offered without unit counts.",
                &[("level", level), ("error", reason)],
            );
            assert!(h.query_by_label(&note).is_some(), "{note}");
            assert!(level_info(&h.state().ui.portals, level).0);
        }
    }

    /// The boundaries geoBoundaries' description names are read within the reach the description
    /// was read at: only from the internet when it came from there; from a description read on the
    /// local network (a mirror at its name), from a private address too.
    #[test]
    fn boundaries_are_read_within_the_reach_of_their_description() {
        for (n, observed) in [(1, Reach::Public), (2, Reach::Local)] {
            let _turn = crate::fetching::turn();
            let geojson = if observed == Reach::Public {
                format!("https://boundaries-{n}.example/che.geojson")
            } else {
                format!("http://10.0.{n}.2/che.geojson")
            };
            let meta =
                json!({"boundaryISO": "CHE", "boundaryName": "Switzerland", "boundaryType": "ADM1", "gjDownloadURL": geojson}).to_string();
            let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let log = asked.clone();
            cache::set_fetcher_with_origin(move |url, ceiling, done, _| {
                let clean = cache::split_download_limit(&url).0.to_string();
                log.lock().unwrap().push((clean.clone(), ceiling));
                let origin = Origin::with_observed_reach(&clean, observed.min(ceiling)).unwrap();
                let body =
                    if clean.ends_with(".geojson") { r#"{"type":"FeatureCollection","features":[]}"#.to_string() } else { meta.clone() };
                done(Ok(cache::Fetched { bytes: body.into_bytes(), origin }));
            });
            let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
            let url = boundaries::meta_url("CHE", "ADM1");
            app.ui.portals.waiting = Some((Asked::Meta(json!({"country": "CHE", "level": "ADM1"})), fetch(&url, Reach::Local)));
            poll(&mut app, &egui::Context::default());
            let asked = asked.lock().unwrap().clone();
            assert_eq!(asked, [(url, Reach::Local), (geojson.clone(), observed)], "observed {observed:?}");
        }
    }
}
