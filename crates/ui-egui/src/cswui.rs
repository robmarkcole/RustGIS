//! Add From Service › Catalogue (CSW): a catalogue searched by words and, optionally, the map's
//! area; its records listed with their titles, abstracts and the links RustGIS can follow — a
//! service (connected to in the same dialog) or a file (downloaded as From URL… does). Requests go
//! off the frame; `catalog.search` reads the answers.

use egui::vec2;
use rustgis_engine::{
    cmd::service,
    render::rustgis_tiles::{address::Reach, cache},
};
use serde_json::{Value, json};

use crate::{RustGisApp, Slot};

/// The catalogue search: its form, the request under way and the records found so far.
#[derive(Default)]
pub struct State {
    pub url: String,
    pub query: String,
    /// Only records within the map's area.
    pub use_view: bool,
    /// The request on its way.
    waiting: Option<Asked>,
    records: Vec<Value>,
    matched: u64,
    next: Option<u64>,
    /// The first page's parameters, which More pages through (not the form, edited since).
    asked: Value,
    notes: Vec<String>,
    pub error: Option<String>,
}

/// A page asked for: `catalog.search`'s parameters, its answer on the way, and the error of the
/// request it is the second try of (`service::records_again`).
struct Asked {
    p: Value,
    answer: Slot<Result<Vec<u8>, String>>,
    refused: Option<String>,
}

/// Links shown for a record.
const SHOWN_LINKS: usize = 6;

/// The link kinds shown, with their labels; `web` pages aren't.
const KINDS: [(&str, &str); 7] = [
    ("wms", "WMS"),
    ("wmts", "WMTS"),
    ("wfs", "WFS"),
    ("wcs", "WCS"),
    ("ogcapi", "OGC API"),
    ("arcgis", "ArcGIS"),
    ("download", "Download"),
];

/// Asks for a page of records: the first (`start` 1) replaces those found, a later one adds to them.
pub fn search(app: &mut RustGisApp, start: u64) {
    let st = &app.ui.csw;
    let mut p = if start <= 1 || !st.asked.is_object() {
        let mut p = json!({"url": st.url.trim(), "query": st.query.trim()});
        // The map's area now, not when More is pressed.
        if st.use_view {
            p["bbox"] =
                rustgis_engine::cmd::catalogs::area(&app.session, &json!({"bbox": "view"})).map_or_else(|_| json!("view"), |a| json!(a));
        }
        p
    } else {
        st.asked.clone()
    };
    if start <= 1 {
        let url = p.get("url").and_then(Value::as_str).unwrap_or("");
        if let Some(id) = crate::serviceui::connection_for(app, app.dialogs.connection.as_deref(), url) {
            p["connection"] = json!(id);
        }
    }
    match rustgis_engine::service_logins::request_params(&mut app.session, &p) {
        Ok(bound) => p = bound.into_owned(),
        Err(e) => {
            app.ui.csw.error = Some(crate::connectionui::translated(&e));
            return;
        }
    }
    let asked = p.clone();
    p["start"] = json!(start);
    match ask(app, p, None) {
        Ok(()) => {
            let st = &mut app.ui.csw;
            st.error = None;
            if start <= 1 {
                st.records.clear();
                st.notes.clear();
                st.matched = 0;
                st.next = None;
                st.asked = asked;
            }
        }
        Err(e) => app.ui.csw.error = Some(e),
    }
}

/// Sends a page's request, its answer read by [`poll`].
fn ask(app: &mut RustGisApp, p: Value, refused: Option<String>) -> Result<(), String> {
    let request = service::records_request(&app.session, &p)?;
    // A catalogue held to the internet is searched only there; an answer read from the
    // internet is recorded, so `catalog.search` holds the links it lists there.
    if p.get("url").and_then(Value::as_str).is_some_and(|url| cache::ceiling_for(url) == Reach::Public) {
        cache::restrict_to_public(&request);
    }
    app.ui.csw.waiting = Some(Asked { p, answer: crate::fetch_document_into(&request), refused });
    Ok(())
}

/// Reads a page of records once it has arrived.
pub fn poll(app: &mut RustGisApp) {
    let Some(Asked { answer, .. }) = &app.ui.csw.waiting else { return };
    let Some(r) = answer.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
    let Some(Asked { mut p, refused, .. }) = app.ui.csw.waiting.take() else { return };
    let r = match r {
        Ok(bytes) => {
            // Before the command reads it: a refusal asked again for is no error yet.
            if let Some((again, refused)) = service::records_again(&app.session, &p, &bytes) {
                if let Err(e) = ask(app, again, Some(refused)) {
                    app.ui.csw.error = Some(e);
                }
                return;
            }
            p["text"] = json!(String::from_utf8_lossy(&bytes));
            // Not `app.run`: an error belongs in the dialog, not the notices.
            app.session.run("catalog.search", &p)
        }
        Err(e) => Err(e),
    };
    match r {
        Ok(v) => {
            let st = &mut app.ui.csw;
            st.matched = v.get("matched").and_then(Value::as_u64).unwrap_or(0);
            st.next = v.get("next").and_then(Value::as_u64);
            st.records.extend(v.get("records").and_then(Value::as_array).into_iter().flatten().cloned());
            // A note about the words or the area is the same on every page: it is listed once. Any
            // other counts what its own page left out, so each is kept.
            let same = service::words_notes(p.get("query").and_then(Value::as_str).unwrap_or(""));
            let page = v.get("notes").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str);
            for note in refused.map(|_| service::AREA_UNNAMED).into_iter().chain(page) {
                let repeats = [service::AREA_UNNAMED, service::AREA_ACROSS_ANTIMERIDIAN].contains(&note) || same.iter().any(|s| s == note);
                if !(repeats && st.notes.iter().any(|n| n == note)) {
                    st.notes.push(note.to_string());
                }
            }
            // More asks for the area as this page's was read.
            if let Some(crs) = v.get("bboxCrs") {
                st.asked["bboxCrs"] = crs.clone();
            }
        }
        Err(e) => {
            app.ui.csw.error = Some(match refused {
                Some(first) => service::refused_again(&first, &e),
                None => e,
            });
        }
    }
}

/// The catalogue form (the Type row is the dialog's) and its records.
pub fn form(app: &mut RustGisApp, ui: &mut egui::Ui, kinds: impl FnOnce(&mut RustGisApp, &mut egui::Ui)) {
    let mut go = false;
    egui::Grid::new("csw").num_columns(2).spacing(vec2(10.0, 8.0)).show(ui, |ui| {
        let input_label = ui.label(tl!("Address"));
        let r = ui
            .add(egui::TextEdit::singleline(&mut app.ui.csw.url).hint_text("https://…/csw").desired_width(420.0))
            .labelled_by(input_label.id);
        go |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        ui.end_row();
        kinds(app, ui);
        ui.end_row();
        let input_label = ui.label(tl!("Words"));
        let r = ui
            .add(egui::TextEdit::singleline(&mut app.ui.csw.query).hint_text(tl!("anywhere in the records")).desired_width(420.0))
            .labelled_by(input_label.id);
        go |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        ui.end_row();
        ui.label("");
        ui.checkbox(&mut app.ui.csw.use_view, tl!("Only where the map is"));
        ui.end_row();
    });
    let t = crate::theme::Tokens::get(ui.ctx());
    ui.horizontal(|ui| {
        go |= crate::widgets::dialog_button(ui, tl!("Search"), Some(t.accent), vec2(96.0, 28.0)).clicked();
        if app.ui.csw.waiting.is_some() {
            ui.spinner();
        }
    });
    if go {
        if app.ui.csw.url.trim().is_empty() {
            app.ui.csw.error = Some(tl!("Give the catalogue's web address").to_string());
        } else {
            search(app, 1);
        }
    }
    if let Some(e) = &app.ui.csw.error {
        ui.colored_label(t.red, e);
    }
    records(app, ui);
}

/// The records found: each with its abstract (on hover), a Zoom to its bounds and a button per
/// link RustGIS can follow.
fn records(app: &mut RustGisApp, ui: &mut egui::Ui) {
    let list = crate::sourcespane::connections_snapshot(app);
    let st = &app.ui.csw;
    if st.records.is_empty() && st.matched == 0 {
        // Nothing matched, but how the search was asked is still said.
        if !st.notes.is_empty() {
            ui.separator();
        }
        for n in &st.notes {
            ui.label(egui::RichText::new(n).small().weak());
        }
        return;
    }
    ui.separator();
    // The address as typed: one read through a login is matched and kept without its binding.
    let url = rustgis_engine::data::login_address::inner(st.asked["url"].as_str().unwrap_or("")).to_string();
    let keepable = !url.is_empty() && crate::connectionui::picked(&list, app.dialogs.connection.as_deref(), &url).is_none();
    let mut keep = false;
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(crate::i18n::fmt(
                "{shown} of {matched} records",
                &[("shown", &st.records.len().to_string()), ("matched", &st.matched.to_string())],
            ))
            .strong(),
        );
        if keepable {
            keep = crate::connectionui::keep_button(ui);
        }
    });
    for n in &st.notes {
        ui.label(egui::RichText::new(n).small().weak());
    }
    let (mut zoom, mut follow, mut more) = (None, None, false);
    egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
        for r in &st.records {
            let title = r.get("title").and_then(Value::as_str).unwrap_or("");
            let summary = r.get("abstract").and_then(Value::as_str).unwrap_or("");
            let label = ui.add(egui::Label::new(egui::RichText::new(title).strong()).truncate());
            if !summary.is_empty() {
                label.on_hover_text(summary.chars().take(600).collect::<String>());
            }
            ui.horizontal_wrapped(|ui| {
                if let Some(b) = r.get("bbox").filter(|b| b.is_array())
                    && ui.small_button(tl!("Zoom")).clicked()
                {
                    zoom = Some(b.clone());
                }
                let mut any = false;
                let links = r.get("links").and_then(Value::as_array).into_iter().flatten();
                for l in links.filter(|l| l.get("kind").and_then(Value::as_str).is_some_and(|k| k != "web")).take(SHOWN_LINKS) {
                    let kind = l.get("kind").and_then(Value::as_str).unwrap_or("");
                    let Some((_, label)) = KINDS.iter().find(|(k, _)| *k == kind) else { continue };
                    let url = l.get("url").and_then(Value::as_str).unwrap_or("");
                    any = true;
                    // The link's name, else the file or path it ends in: links of a kind told apart.
                    let path = url.split(['?', '#']).next().unwrap_or(url).trim_end_matches('/');
                    let what = l
                        .get("name")
                        .and_then(Value::as_str)
                        .filter(|n| !n.is_empty())
                        .unwrap_or_else(|| path.rsplit('/').next().unwrap_or(""));
                    let what: String = what.chars().take(28).collect();
                    let text = if what.is_empty() { tl!(*label).to_string() } else { format!("{} · {what}", tl!(*label)) };
                    if ui.small_button(text).on_hover_text(url).clicked() {
                        let public = l.get("fromDocument").and_then(Value::as_bool) == Some(true);
                        follow = Some((kind.to_string(), url.to_string(), title.to_string(), public));
                    }
                }
                if !any {
                    ui.label(egui::RichText::new(tl!("no service or file links")).small().weak());
                }
            });
            ui.add_space(4.0);
        }
        if st.next.is_some() && st.waiting.is_none() && ui.button(tl!("More")).clicked() {
            more = true;
        }
    });
    if more {
        let next = app.ui.csw.next.unwrap_or(1);
        search(app, next);
    }
    if let Some(b) = zoom {
        let _ = app.run("map.zoomToExtent", json!({"extent": b}));
    }
    if let Some((kind, url, title, public)) = follow {
        app.dialogs.error = None;
        // A link from a catalogue read from the internet leads only there (`fromDocument`).
        let r = if kind == "download" {
            app.run("ui.addUrl", json!({"url": url, "name": title, "fromDocument": public})).map(|_| ())
        } else {
            // A service: connected to in this dialog, its type picked (the catalogue's results
            // stay for Catalogue).
            app.dialogs.mode = match kind.as_str() {
                "wms" => "wms",
                "wmts" => "wmts",
                "wfs" => "wfs",
                "wcs" => "wcs",
                "ogcapi" => "ogcapi",
                _ => "arcgis",
            };
            app.run("ui.addService", json!({"url": url, "type": app.dialogs.mode, "fromDocument": public})).map(|_| ())
        };
        if let Err(e) = r {
            app.ui.csw.error = Some(e);
        }
    }
    if keep {
        match crate::connectionui::keep(app, json!({"kind": "csw", "address": url})) {
            Ok(id) => app.dialogs.connection = Some(id),
            Err(e) => app.ui.csw.error = Some(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_engine::render::rustgis_tiles::{
        address::{Origin, Reach},
        cache,
    };

    /// A catalogue's answer read from the internet holds the services its records link to there:
    /// clicking a record's service reads its capabilities only from the internet. Read on the local
    /// network, at a public-looking name, its records keep a service on a private address, and
    /// clicking it reads that service as the user's own.
    #[test]
    fn a_catalogues_services_are_read_within_the_reach_of_its_answer() {
        use egui_kittest::kittest::Queryable;
        for (n, observed) in [(1, Reach::Public), (2, Reach::Local)] {
            let _turn = crate::fetching::turn();
            let csw = format!("https://csw-{n}.corp-looks-public.example/csw");
            let service =
                if observed == Reach::Public { format!("https://csw-service-{n}.example/wms") } else { format!("http://10.0.{n}.4/wms") };
            let answer = format!(
                r#"<csw:GetRecordsResponse xmlns:csw="http://www.opengis.net/cat/csw/2.0.2" xmlns:dc="http://purl.org/dc/elements/1.1/"><csw:SearchResults numberOfRecordsMatched="1"><csw:Record><dc:identifier>a</dc:identifier><dc:title>Roads</dc:title><dc:URI protocol="OGC:WMS">{service}</dc:URI></csw:Record></csw:SearchResults></csw:GetRecordsResponse>"#
            );
            let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let log = asked.clone();
            cache::set_fetcher_with_origin(move |url, ceiling, done, _| {
                let clean = cache::split_download_limit(&url).0.to_string();
                log.lock().unwrap().push((clean.clone(), ceiling));
                let origin = Origin::with_observed_reach(&clean, observed.min(ceiling)).unwrap();
                done(Ok(cache::Fetched { bytes: answer.clone().into_bytes(), origin }));
            });
            let app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
            let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1400.0, 900.0)).with_max_steps(16).build_ui_state(
                |ui, app: &mut RustGisApp| {
                    let ctx = ui.ctx().clone();
                    app.logic(&ctx);
                    app.ui(ui);
                },
                app,
            );
            h.state_mut().run("ui.addService", json!({"url": csw, "type": "csw"})).unwrap();
            h.run_steps(3);
            assert_eq!(h.state().ui.csw.records.len(), 1, "{:?}", h.state().ui.csw.error);
            h.get_by_label("WMS · wms").click();
            h.run_steps(2);
            let caps = rustgis_engine::data::ogc::capabilities_url(&service, Some("wms"));
            let asked = asked.lock().unwrap().clone();
            assert!(asked.contains(&(caps.clone(), observed)), "the service, its catalogue read {observed:?}: {asked:?}");
            assert_eq!(cache::ceiling_for(&caps), observed);
        }
    }

    /// A catalogue a document read from the internet named (`fromDocument`) is searched only there.
    #[test]
    fn a_catalogue_a_document_named_is_searched_only_on_the_internet() {
        for public in [true, false] {
            let _turn = crate::fetching::turn();
            let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let log = asked.clone();
            cache::set_fetcher_with_origin(move |url, ceiling, done, _| {
                log.lock().unwrap().push(ceiling);
                let origin = Origin::with_observed_reach(cache::split_download_limit(&url).0, ceiling).unwrap();
                done(Ok(cache::Fetched {
                    bytes: b"<csw:GetRecordsResponse xmlns:csw=\"http://www.opengis.net/cat/csw/2.0.2\"/>".to_vec(),
                    origin,
                }));
            });
            let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
            let csw = format!("https://csw-named-{public}.example/csw");
            app.run("ui.addService", json!({"url": csw, "type": "csw", "fromDocument": public})).unwrap();
            let want = if public { Reach::Public } else { Reach::Local };
            assert_eq!(asked.lock().unwrap().clone(), [want]);
        }
    }

    /// Only where the map is: a catalogue that answers the area's CRS84 with an exception is asked
    /// once more without it, which is said, and More asks as that page was read; one that answers
    /// it is asked once. One that refuses both shows the server's words (#2889).
    #[test]
    fn an_area_refused_with_its_system_named_is_asked_for_again_without() {
        const REFUSED: &str = "<ows:ExceptionReport xmlns:ows='o'><ows:Exception><ows:ExceptionText>no such system</ows:ExceptionText></ows:Exception></ows:ExceptionReport>";
        const PAGE: &str = r#"<csw:GetRecordsResponse xmlns:csw="http://www.opengis.net/cat/csw/2.0.2" xmlns:dc="http://purl.org/dc/elements/1.1/"><csw:SearchResults numberOfRecordsMatched="40" nextRecord="21"><csw:Record><dc:title>Roads</dc:title></csw:Record></csw:SearchResults></csw:GetRecordsResponse>"#;
        for (n, named, unnamed) in [(1, true, true), (2, false, true), (3, false, false)] {
            let _turn = crate::fetching::turn();
            let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let log = asked.clone();
            cache::set_fetcher_with_origin(move |url, ceiling, done, _| {
                let is_named = url.contains("CRS84");
                log.lock().unwrap().push((is_named, url.contains("&startPosition=21&"), ceiling));
                let body = if if is_named { named } else { unnamed } { PAGE } else { REFUSED };
                let origin = Origin::with_observed_reach(cache::split_download_limit(&url).0, ceiling).unwrap();
                done(Ok(cache::Fetched { bytes: body.into(), origin }));
            });
            let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
            app.session.run("map.zoomToExtent", &json!({"extent": [-43, -21, -42, -20]})).unwrap();
            let csw = format!("https://csw-area-{n}.example/csw");
            // Held to the internet, which the second request follows too.
            app.run("ui.addService", json!({"url": csw, "type": "csw", "query": "roads", "bbox": "view", "fromDocument": true})).unwrap();
            poll(&mut app);
            poll(&mut app);
            let public = Reach::Public;
            let st = &app.ui.csw;
            assert!(st.waiting.is_none());
            if !named && !unnamed {
                assert_eq!(asked.lock().unwrap().clone(), [(true, false, public), (false, false, public)]);
                let error = st.error.as_deref().unwrap_or_default();
                assert!(error.ends_with("the catalogue answered with an error: no such system"), "{error}");
                assert!(st.records.is_empty() && st.notes.is_empty());
                continue;
            }
            assert_eq!((st.error.as_deref(), st.records.len(), st.next), (None, 1, Some(21)));
            // The refusal asked again for isn't an error in the status bar either.
            assert!(!app.session.notices.iter().any(|n| n.error));
            let said = if named { vec![] } else { vec![service::AREA_UNNAMED.to_string()] };
            assert_eq!(st.notes, said);
            search(&mut app, 21);
            poll(&mut app);
            let first: &[_] = if named { &[(true, false, public)] } else { &[(true, false, public), (false, false, public)] };
            assert_eq!(asked.lock().unwrap().clone(), [first, &[(named, true, public)]].concat());
            let st = &app.ui.csw;
            assert_eq!((st.error.as_deref(), st.records.len(), &st.notes), (None, 2, &said));
        }
    }

    /// A note about the words (a control character left out) is the same on every page: More
    /// doesn't list it again.
    #[test]
    fn a_note_about_the_words_is_listed_once_over_pages() {
        const PAGE: &str = r#"<csw:GetRecordsResponse xmlns:csw="http://www.opengis.net/cat/csw/2.0.2" xmlns:dc="http://purl.org/dc/elements/1.1/"><csw:SearchResults numberOfRecordsMatched="40" nextRecord="21"><csw:Record><dc:title>Roads</dc:title></csw:Record></csw:SearchResults></csw:GetRecordsResponse>"#;
        let _turn = crate::fetching::turn();
        cache::set_fetcher_with_origin(move |url, ceiling, done, _| {
            let origin = Origin::with_observed_reach(cache::split_download_limit(&url).0, ceiling).unwrap();
            done(Ok(cache::Fetched { bytes: PAGE.into(), origin }));
        });
        let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
        app.run("ui.addService", json!({"url": "https://csw-words.example/csw", "type": "csw", "query": "ro\u{7}ads"})).unwrap();
        poll(&mut app);
        let said = vec!["control characters left out of the words searched for: 1".to_string()];
        assert_eq!((app.ui.csw.records.len(), &app.ui.csw.notes), (1, &said));
        search(&mut app, 21);
        poll(&mut app);
        assert_eq!((app.ui.csw.records.len(), &app.ui.csw.notes), (2, &said));
    }

    /// An area across the antimeridian, left out of every page's request, is said once over pages.
    #[test]
    fn an_area_across_the_antimeridian_is_said_once_over_pages() {
        const PAGE: &str = r#"<csw:GetRecordsResponse xmlns:csw="http://www.opengis.net/cat/csw/2.0.2" xmlns:dc="http://purl.org/dc/elements/1.1/"><csw:SearchResults numberOfRecordsMatched="40" nextRecord="21"><csw:Record><dc:title>Roads</dc:title></csw:Record></csw:SearchResults></csw:GetRecordsResponse>"#;
        let _turn = crate::fetching::turn();
        cache::set_fetcher_with_origin(move |url, ceiling, done, _| {
            let origin = Origin::with_observed_reach(cache::split_download_limit(&url).0, ceiling).unwrap();
            done(Ok(cache::Fetched { bytes: PAGE.into(), origin }));
        });
        let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
        // The search More pages through, its area across the antimeridian.
        app.ui.csw.asked = json!({"url": "https://csw-pacific.example/csw", "query": "roads", "bbox": [170, -20, -170, -10]});
        for page in [1, 2] {
            search(&mut app, 1 + 20 * page);
            poll(&mut app);
            assert_eq!(app.ui.csw.records.len(), page as usize);
        }
        assert_eq!(app.ui.csw.notes, [service::AREA_ACROSS_ANTIMERIDIAN.to_string()]);
    }

    /// A note that counts what one page left out is said for every page, even when two pages
    /// leave out as many: More keeps both, so nothing is under-counted.
    #[test]
    fn a_note_counting_a_pages_links_is_kept_for_every_page() {
        const PAGE: &str = r#"<csw:GetRecordsResponse xmlns:csw="http://www.opengis.net/cat/csw/2.0.2" xmlns:dc="http://purl.org/dc/elements/1.1/"><csw:SearchResults numberOfRecordsMatched="40" nextRecord="21"><csw:Record><dc:title>Roads</dc:title><dc:URI protocol="OGC:WMS">http://10.0.0.4/wms</dc:URI><dc:URI protocol="OGC:WFS">http://10.0.0.5/wfs</dc:URI></csw:Record></csw:SearchResults></csw:GetRecordsResponse>"#;
        let _turn = crate::fetching::turn();
        cache::set_fetcher_with_origin(move |url, ceiling, done, _| {
            let origin = Origin::with_observed_reach(cache::split_download_limit(&url).0, ceiling).unwrap();
            done(Ok(cache::Fetched { bytes: PAGE.into(), origin }));
        });
        let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
        // Held to the internet: links to the local network are left out.
        app.run(
            "ui.addService",
            json!({"url": "https://csw-links.example/csw", "type": "csw", "query": "ro\u{7}ads", "fromDocument": true}),
        )
        .unwrap();
        poll(&mut app);
        let words = "control characters left out of the words searched for: 1".to_string();
        let links = "2 links point to addresses RustGIS won't fetch for a catalogue on the web and are left out".to_string();
        assert_eq!((app.ui.csw.records.len(), &app.ui.csw.notes), (1, &vec![words.clone(), links.clone()]));
        search(&mut app, 21);
        poll(&mut app);
        assert_eq!((app.ui.csw.records.len(), &app.ui.csw.notes), (2, &vec![words, links.clone(), links]));
    }

    /// A search that matched nothing still says how it was asked: the area asked for again without
    /// its system is shown with no records (#2889).
    #[test]
    fn a_search_that_matched_nothing_shows_its_notes() {
        use egui_kittest::kittest::Queryable;
        const REFUSED: &str = "<ows:ExceptionReport xmlns:ows='o'><ows:Exception><ows:ExceptionText>no such system</ows:ExceptionText></ows:Exception></ows:ExceptionReport>";
        const NONE: &str = r#"<csw:GetRecordsResponse xmlns:csw="http://www.opengis.net/cat/csw/2.0.2"><csw:SearchResults numberOfRecordsMatched="0" nextRecord="0"/></csw:GetRecordsResponse>"#;
        let _turn = crate::fetching::turn();
        cache::set_fetcher_with_origin(move |url, ceiling, done, _| {
            let body = if url.contains("CRS84") { REFUSED } else { NONE };
            let origin = Origin::with_observed_reach(cache::split_download_limit(&url).0, ceiling).unwrap();
            done(Ok(cache::Fetched { bytes: body.into(), origin }));
        });
        let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
        app.session.run("map.zoomToExtent", &json!({"extent": [-43, -21, -42, -20]})).unwrap();
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1400.0, 900.0)).with_max_steps(16).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            app,
        );
        h.state_mut()
            .run("ui.addService", json!({"url": "https://csw-nothing.example/csw", "type": "csw", "query": "roads", "bbox": "view"}))
            .unwrap();
        h.run_steps(4);
        let st = &h.state().ui.csw;
        assert_eq!((st.error.as_deref(), st.records.len(), st.matched), (None, 0, 0));
        assert_eq!(st.notes, [service::AREA_UNNAMED.to_string()]);
        assert!(h.query_by_label(service::AREA_UNNAMED).is_some(), "the note isn't shown");
    }
}
