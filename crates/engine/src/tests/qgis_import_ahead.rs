//! Import QGIS Project run off the window (#2476): in the desktop app the import runs on a copy of
//! the session, on a thread of its own, and the map it made is taken when it lands. The map taken is
//! the one the import makes when it runs here; a saved web login is the session's, borrowed and
//! left open; a cancelled import leaves nothing behind (its sweep is in `cancellation.rs`).

use super::{Project, Session, ogc_server};
use ogc_server::{Route, Server};
use rustgis_analysis::control::Control;
use serde_json::{Value, json};

/// A folder of its own for one test, removed when it ends.
struct Folder(std::path::PathBuf);

impl Folder {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("rustgis-qgis-ahead-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A WFS of three roads.
fn roads_server() -> Server {
    let features: Vec<_> = [1, 2, 3]
        .iter()
        .map(|id| json!({"type": "Feature", "id": id, "properties": {"n": id}, "geometry": {"type": "Point", "coordinates": [*id, 1]}}))
        .collect();
    let page = json!({"type": "FeatureCollection", "features": features}).to_string();
    let query = "SERVICE=WFS&VERSION=2.0.0&REQUEST=GetFeature&TYPENAMES=roads";
    let format = "OUTPUTFORMAT=application%2Fjson&SRSNAME=EPSG:4326";
    let server = Server::new(vec![
        Route::new("/wfs?SERVICE=WFS&REQUEST=GetCapabilities", ""),
        Route::new(&format!("/wfs?{query}&{format}&COUNT=50000"), &page),
        Route::new(&format!("/wfs?{query}&SRSNAME=EPSG:4326&RESULTTYPE=hits"), r#"<FeatureCollection numberMatched="3"/>"#),
    ]);
    let base = server.base.clone();
    server.set(
        "/wfs?SERVICE=WFS&REQUEST=GetCapabilities",
        &format!(
            r#"<WFS_Capabilities version="2.0.0"><OperationsMetadata><Operation name="GetFeature"><DCP><HTTP><Get href="{base}/wfs?"/></HTTP></DCP></Operation></OperationsMetadata><FeatureTypeList><FeatureType><Name>roads</Name><Title>Roads</Title></FeatureType></FeatureTypeList></WFS_Capabilities>"#
        ),
    );
    server
}

/// A project in `dir` with a WFS layer at `base`, a GeoJSON file beside it, a CSV without geometry
/// (a table), a PostGIS table (a saved connection made for it), a group holding the file's layer,
/// and a bookmark.
fn project(dir: &Folder, base: &str) -> std::path::PathBuf {
    let towns = json!({"type": "FeatureCollection", "features": [
        {"type": "Feature", "properties": {"name": "A"}, "geometry": {"type": "Point", "coordinates": [8.5, 47.4]}},
        {"type": "Feature", "properties": {"name": "B"}, "geometry": {"type": "Point", "coordinates": [7.4, 46.9]}}]});
    std::fs::write(dir.0.join("towns.geojson"), towns.to_string()).unwrap();
    // Without a geometry field: a table of its own.
    std::fs::write(dir.0.join("notes.csv"), "id,note\n1,first\n2,second\n").unwrap();
    let layer = |id: &str, kind: &str, provider: &str, source: &str| {
        format!(
            r#"<maplayer type="{kind}"><id>{id}</id><datasource>{source}</datasource><layername>{id}</layername><provider>{provider}</provider></maplayer>"#
        )
    };
    let qgs = format!(
        r#"<qgis version="4.2.3" projectname="Ahead"><title>Ahead</title><layer-tree-group>
            <layer-tree-group name="Places" checked="Qt::Checked" expanded="1"><layer-tree-layer id="Towns" checked="Qt::Checked"/></layer-tree-group>
            <layer-tree-layer id="Roads" checked="Qt::Checked"/><layer-tree-layer id="Parcels" checked="Qt::Checked"/>
            <layer-tree-layer id="Notes" checked="Qt::Checked"/>
        </layer-tree-group><projectlayers>{}{}{}{}</projectlayers>
        <Bookmarks><Bookmark id="b" name="Zurich" group="" extent="POLYGON((8.4 47.3, 8.6 47.3, 8.6 47.5, 8.4 47.5, 8.4 47.3))"><spatialrefsys><authid>EPSG:4326</authid></spatialrefsys></Bookmark></Bookmarks></qgis>"#,
        layer("Towns", "vector", "ogr", "./towns.geojson"),
        layer("Roads", "vector", "WFS", &format!("typename='roads' url='{base}/wfs' version='2.0.0'")),
        layer("Notes", "vector", "delimitedtext", "file:./notes.csv?type=csv&amp;geomType=none"),
        layer(
            "Parcels",
            "vector",
            "postgres",
            "dbname='gis' host=db.example.invalid port=5432 user='reader' table=\"public\".\"parcels\" (geom)"
        ),
    );
    let path = dir.0.join("ahead.qgs");
    std::fs::write(&path, qgs).unwrap();
    path
}

/// `project.importQgis` with `p`, run as the desktop app runs it: on a copy of the session, on a
/// thread of its own (here, this one), and taken when it lands.
fn import_ahead(s: &mut Session, p: &Value, control: &std::sync::Arc<Control>) -> crate::CmdResult {
    let read = crate::cmd::reads::read_ahead(s, "project.importQgis", p)?;
    assert!(read.reads(), "the import runs on a copy");
    let done = read.run(control)?;
    crate::cmd::reads::read_land(s, done)
}

#[test]
fn an_import_run_on_a_copy_makes_the_map_an_import_here_makes() {
    let dir = Folder::new("same");
    let server = roads_server();
    let path = project(&dir, &server.base);
    let p = json!({"path": path.display().to_string()});
    let mut here = Session::new(Project::default());
    let answer_here = here.run("project.importQgis", &p).unwrap();
    let mut ahead = Session::new(Project::default());
    let answer_ahead = import_ahead(&mut ahead, &p, &Control::new()).unwrap();
    assert_eq!(answer_ahead, answer_here);
    assert_eq!(answer_ahead["layers"].as_array().map(Vec::len), Some(2), "{answer_ahead}");
    // The CSV without geometry, a table here as when the import runs here (which notes its layer as
    // not added).
    assert_eq!(ahead.project.tables.len(), 1);
    let tables = |s: &Session| s.project.tables.iter().map(|t| (t.id, t.dataset.name.clone())).collect::<Vec<_>>();
    assert_eq!(tables(&ahead), tables(&here));
    // The whole project as saved, its maps, layers, groups and bookmarks among it.
    assert!(super::cancellation::contents(&ahead.project) == super::cancellation::contents(&here.project));
    assert_eq!(ahead.project.active_map, here.project.active_map);
    assert_eq!(ahead.active_layer, here.active_layer);
    // The PostGIS table's saved connection, made here once the copy has landed, without a password.
    assert_eq!(ahead.connections.connections, here.connections.connections);
    assert_eq!(ahead.connections.connections.len(), 1);
    // What it said is said here, as when it runs here.
    let said = |s: &Session| s.notices.iter().map(|n| n.text.clone()).collect::<Vec<_>>();
    assert_eq!(said(&ahead), said(&here));
    // One Undo step takes the map away again.
    let maps = ahead.project.maps.len();
    ahead.run("edit.undo", &json!({})).unwrap();
    assert_eq!(ahead.project.maps.len(), maps - 1);
}

/// A map named as the project is named again when a map took the name while it was read, as a new
/// map is; saved connections changed meanwhile refuse it, to be imported again.
#[test]
fn an_import_on_a_copy_lands_on_the_session_as_it_is_then() {
    let dir = Folder::new("then");
    let server = roads_server();
    let path = project(&dir, &server.base);
    let p = json!({"path": path.display().to_string()});
    let mut s = Session::new(Project::default());
    let read = crate::cmd::reads::read_ahead(&mut s, "project.importQgis", &p).unwrap();
    let done = read.run(&Control::new()).unwrap();
    // (Renamed meanwhile: a map made or opened meanwhile is a change of map, which the read refuses.)
    s.map_mut().name = "Ahead".into();
    let answer = crate::cmd::reads::read_land(&mut s, done).unwrap();
    assert_eq!(answer["map"], "Ahead2");
    assert_eq!(s.map().name, "Ahead2");
    assert!(s.map().layer_by_name("Towns").is_some());
    let mut s = Session::new(Project::default());
    let read = crate::cmd::reads::read_ahead(&mut s, "project.importQgis", &p).unwrap();
    let done = read.run(&Control::new()).unwrap();
    s.run("connection.new", &json!({"kind": "postgres", "address": "postgres:host=db2.example.invalid dbname=other"})).unwrap();
    let maps = s.project.maps.len();
    let e = crate::cmd::reads::read_land(&mut s, done).unwrap_err();
    // (Refused when it lands: by the read, which sees the account changed, or by the import.)
    assert!(e.contains("changed"), "{e}");
    assert_eq!(s.project.maps.len(), maps);
    assert_eq!(s.connections.connections.len(), 1);
}

/// A service's saved login: the copy sends it as this session would, borrowing this session's
/// login, which stays open after the copy is gone; a login this session couldn't open isn't
/// opened by the copy either.
#[test]
fn an_import_on_a_copy_borrows_the_sessions_web_login() {
    let dir = Folder::new("login");
    let server = roads_server();
    let path = project(&dir, &server.base);
    let p = json!({"path": path.display().to_string()});
    let mut s = Session::new(Project::default());
    let id = s
        .run(
            "connection.new",
            &json!({"kind": "wfs", "name": "Roads", "address": format!("{}/wfs", server.base), "auth": "query", "keyName": "key"}),
        )
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    s.run("connection.login", &json!({"id": id, "apiKey": "RGSYNTH_ahead_key", "allowInsecure": true})).unwrap();
    let keyed =
        |server: &Server| server.requests().iter().filter(|r| r.query.iter().any(|(k, v)| k == "key" && v == "RGSYNTH_ahead_key")).count();
    // (The fixture answers only the addresses it was given, so the keyed ones go unanswered and the
    // layer is left out: what matters is what was sent.)
    import_ahead(&mut s, &p, &Control::new()).unwrap();
    let sent = keyed(&server);
    assert!(sent > 0, "the copy sent the session's login");
    // Still open here: a request now carries it, with no login asked for again.
    let c = s.connections.connections.iter().find(|c| c.id == id).unwrap().clone();
    assert!(crate::service_logins::logged_in(&s, &c));
    // Logged out, the session can't open it, and neither does the copy: the layer is left out,
    // saying why, and nothing is sent with the key.
    s.run("connection.logout", &json!({"id": id})).unwrap();
    let answer = import_ahead(&mut s, &p, &Control::new()).unwrap();
    let notes = serde_json::to_string(&answer["notes"]).unwrap();
    assert!(notes.contains(crate::service_logins::LOGGED_OUT), "{notes}");
    assert_eq!(keyed(&server), sent);
}

/// A login saved on this computer and not yet opened this session: opened for the import before
/// it starts, so the import lands (its accounts as they were when it started) and the copy sends
/// it.
#[test]
fn an_import_on_a_copy_opens_a_saved_login_before_it_starts() {
    use rustgis_data::secrets::MemorySecrets;
    let dir = Folder::new("saved");
    let server = roads_server();
    let path = project(&dir, &server.base);
    let p = json!({"path": path.display().to_string()});
    let mut s = Session::new(Project::default());
    s.database_logins.set_store(Some(std::sync::Arc::new(MemorySecrets::new())));
    let id = s
        .run(
            "connection.new",
            &json!({"kind": "wfs", "name": "Roads", "address": format!("{}/wfs", server.base), "auth": "query", "keyName": "key"}),
        )
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    s.run("connection.login", &json!({"id": id, "apiKey": "RGSYNTH_saved_key", "allowInsecure": true, "remember": true})).unwrap();
    // Saved, but not open this session.
    let c = s.connections.connections.iter().find(|c| c.id == id).unwrap().clone();
    crate::service_logins::deactivate(&mut s, &c, None);
    s.database_logins.clear_session_password(&id);
    assert!(!crate::service_logins::logged_in(&s, &c));
    import_ahead(&mut s, &p, &Control::new()).unwrap();
    assert!(server.requests().iter().any(|r| r.query.iter().any(|(k, v)| k == "key" && v == "RGSYNTH_saved_key")));
    assert!(crate::service_logins::logged_in(&s, &c), "opened here, and still open");
}

/// A borrowing copy opens no login, even one saved where it could read it: it sends only what its
/// session opened for it, and refuses the rest, saying why.
#[test]
fn a_borrowing_copy_opens_no_login_of_its_own() {
    use rustgis_data::secrets::MemorySecrets;
    let store = std::sync::Arc::new(MemorySecrets::new());
    let mut s = Session::new(Project::default());
    s.database_logins.set_store(Some(store.clone()));
    let origin = "https://borrowed.example.invalid";
    let id =
        s.run("connection.new", &json!({"kind": "wfs", "name": "B", "address": origin, "auth": "query", "keyName": "key"})).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
    s.run("connection.login", &json!({"id": id, "apiKey": "RGSYNTH_borrowed", "remember": true})).unwrap();
    let c = s.connections.connections.iter().find(|c| c.id == id).unwrap().clone();
    crate::service_logins::deactivate(&mut s, &c, None);
    s.database_logins.clear_session_password(&id);
    // Borrowed for no origin: the copy has none open, though the store it reaches holds one.
    let mut copy = Session::new(Project::default());
    copy.database_logins.set_store(Some(store));
    copy.connections = s.connections.clone();
    copy.web_logins = s.web_logins.borrowing(&Default::default(), [(origin.to_string(), "why".to_string())].into());
    let url = format!("{origin}/wfs");
    assert_eq!(crate::service_logins::request(&mut copy, None, &url).unwrap_err(), "why");
    assert!(!crate::service_logins::logged_in(&copy, &c));
}

/// A session with a saved login for a WFS at `base`, not open this session, and a tile layer of
/// its connection: the connection's id and the tile's address.
fn saved_login_with_a_tile(s: &mut Session, base: &str) -> (String, String) {
    s.database_logins.set_store(Some(std::sync::Arc::new(rustgis_data::secrets::MemorySecrets::new())));
    let made = s.run(
        "connection.new",
        &json!({"kind": "wfs", "name": "Roads", "address": format!("{base}/wfs"), "auth": "query", "keyName": "key"}),
    );
    let id = made.unwrap()["id"].as_str().unwrap().to_string();
    s.run("connection.login", &json!({"id": id, "apiKey": "RGSYNTH_held_key", "allowInsecure": true, "remember": true})).unwrap();
    let c = s.connections.connections.iter().find(|c| c.id == id).unwrap().clone();
    crate::service_logins::deactivate(s, &c, None);
    s.database_logins.clear_session_password(&id);
    let mut t = rustgis_map::TileLayer::new("Tiles", rustgis_map::TileKind::Raster, format!("{base}/tiles/{{z}}/{{x}}/{{y}}.png"));
    t.connection.clone_from(&id);
    s.map_mut().tiles.push(t);
    let url = s.map().tiles[0].url.clone();
    (id, url)
}

/// A saved login opened for an import leaves the layers and tiles already using its connection as
/// they are while the import runs (#3120): they take it when the import lands.
#[test]
fn a_login_opened_for_an_import_rebinds_its_tiles_only_as_it_lands() {
    let dir = Folder::new("held");
    let server = roads_server();
    let path = project(&dir, &server.base);
    let p = json!({"path": path.display().to_string()});
    let mut s = Session::new(Project::default());
    let (id, before) = saved_login_with_a_tile(&mut s, &server.base);
    let c = s.connections.connections.iter().find(|c| c.id == id).unwrap().clone();
    let read = crate::cmd::reads::read_ahead(&mut s, "project.importQgis", &p).unwrap();
    assert!(crate::service_logins::logged_in(&s, &c), "opened before it starts");
    assert_eq!(s.map().tiles[0].url, before, "the tile waits for the import");
    let done = read.run(&Control::new()).unwrap();
    s.reclaim_rooms();
    assert!(crate::service_logins::logged_in(&s, &c), "kept while the import still holds it");
    crate::cmd::reads::read_land(&mut s, done).unwrap();
    let tile = &s.project.maps[0].tiles[0];
    assert_ne!(tile.url, before, "rebound as it landed");
    assert_eq!(crate::render::rustgis_tiles::logins::inner(&tile.url), before);
    s.reclaim_rooms();
    assert!(crate::service_logins::logged_in(&s, &c), "the session's now");
}

/// An import that never lands (cancelled, refused) ends again the login opened for it, and leaves
/// the tiles of its connection as they were: it leaves nothing behind (#3120).
#[test]
fn a_login_opened_for_an_import_that_never_lands_ends_again() {
    let dir = Folder::new("unheld");
    let server = roads_server();
    let path = project(&dir, &server.base);
    let p = json!({"path": path.display().to_string()});
    let mut s = Session::new(Project::default());
    let (id, before) = saved_login_with_a_tile(&mut s, &server.base);
    let c = s.connections.connections.iter().find(|c| c.id == id).unwrap().clone();
    let read = crate::cmd::reads::read_ahead(&mut s, "project.importQgis", &p).unwrap();
    assert!(crate::service_logins::logged_in(&s, &c));
    drop(read);
    s.reclaim_rooms();
    assert!(!crate::service_logins::logged_in(&s, &c), "ended again");
    assert!(s.database_logins.session_password(&id).is_none());
    assert_eq!(s.map().tiles[0].url, before);
    // Cancelled while it reads: the same.
    let control = Control::new();
    control.cancel();
    let read = crate::cmd::reads::read_ahead(&mut s, "project.importQgis", &p).unwrap();
    assert!(read.run(&control).is_err());
    s.reclaim_rooms();
    assert!(!crate::service_logins::logged_in(&s, &c));
    assert_eq!(s.map().tiles[0].url, before);
    // And it opens again for the next import, which lands.
    import_ahead(&mut s, &p, &Control::new()).unwrap();
    assert!(crate::service_logins::logged_in(&s, &c));
}

/// An import let go before it is prepared (its request refused) holds no login: the one opened
/// for it ends again, and a read prepared holds none past its start.
#[test]
fn an_import_let_go_before_it_starts_holds_no_login() {
    let dir = Folder::new("refused");
    let server = roads_server();
    let path = project(&dir, &server.base);
    let p = json!({"path": path.display().to_string()});
    let mut s = Session::new(Project::default());
    let (id, before) = saved_login_with_a_tile(&mut s, &server.base);
    let c = s.connections.connections.iter().find(|c| c.id == id).unwrap().clone();
    s.import_opened = crate::cmd::qgis::open_import_logins(&mut s, &p);
    assert!(s.import_opened.is_some() && crate::service_logins::logged_in(&s, &c));
    // As `capture_read` lets it go on every way out.
    s.import_opened = None;
    s.reclaim_rooms();
    assert!(!crate::service_logins::logged_in(&s, &c));
    assert_eq!(s.map().tiles[0].url, before);
    let read = crate::cmd::reads::read_ahead(&mut s, "project.importQgis", &p).unwrap();
    assert!(s.import_opened.is_none(), "taken by the read");
    drop(read);
}

/// Saved connections an import made are made here only all together: one that can't be leaves
/// none saved and adds no map (#3120).
#[test]
fn an_import_whose_connections_cant_all_be_saved_saves_none() {
    let dir = Folder::new("connections");
    let server = roads_server();
    let path = project(&dir, &server.base);
    let p = json!({"path": path.display().to_string()});
    let mut s = Session::new(Project::default());
    let read = crate::cmd::reads::read_ahead(&mut s, "project.importQgis", &p).unwrap();
    let done = read.run(&Control::new()).unwrap();
    // Connections from a newer RustGIS, kept whole: none can be added.
    let newer = r#"{"version": 2, "next": 4, "connections": []}"#;
    s.connections = crate::connections::Connections::from_json(newer);
    let maps = s.project.maps.len();
    assert!(crate::cmd::reads::read_land(&mut s, done).is_err());
    assert_eq!(s.project.maps.len(), maps);
    assert_eq!(s.connections.to_json(), newer);
}

/// A login opened for an import that something else here uses meanwhile is this session's from
/// then on: its tiles take it then, and it stays open whether the import lands or not.
#[test]
fn a_login_opened_for_an_import_and_used_meanwhile_stays_open() {
    let dir = Folder::new("adopted");
    let server = roads_server();
    let path = project(&dir, &server.base);
    let p = json!({"path": path.display().to_string()});
    let mut s = Session::new(Project::default());
    let (id, before) = saved_login_with_a_tile(&mut s, &server.base);
    let c = s.connections.connections.iter().find(|c| c.id == id).unwrap().clone();
    let read = crate::cmd::reads::read_ahead(&mut s, "project.importQgis", &p).unwrap();
    crate::service_logins::request(&mut s, None, &format!("{}/wfs", server.base)).unwrap();
    assert_ne!(s.map().tiles[0].url, before, "taken as it is used here");
    drop(read);
    s.reclaim_rooms();
    assert!(crate::service_logins::logged_in(&s, &c));
}

/// A login opened for one import and borrowed by a second stays open while the second runs, when
/// the first is cancelled; it ends only when neither lands.
#[test]
fn a_login_two_imports_hold_stays_open_while_one_runs() {
    let dir = Folder::new("two");
    let server = roads_server();
    let path = project(&dir, &server.base);
    let p = json!({"path": path.display().to_string()});
    let mut s = Session::new(Project::default());
    let (id, _) = saved_login_with_a_tile(&mut s, &server.base);
    let c = s.connections.connections.iter().find(|c| c.id == id).unwrap().clone();
    let first = crate::cmd::reads::read_ahead(&mut s, "project.importQgis", &p).unwrap();
    let second = crate::cmd::reads::read_ahead(&mut s, "project.importQgis", &p).unwrap();
    drop(first);
    s.reclaim_rooms();
    assert!(crate::service_logins::logged_in(&s, &c), "the second still holds it");
    drop(second);
    s.reclaim_rooms();
    assert!(!crate::service_logins::logged_in(&s, &c));
}

/// A login the user opens while an import holds it is the user's: the import's end leaves it open,
/// with the session password they gave.
#[test]
fn a_login_the_user_opens_while_an_import_holds_it_stays_open() {
    let dir = Folder::new("user");
    let server = roads_server();
    let path = project(&dir, &server.base);
    let p = json!({"path": path.display().to_string()});
    let mut s = Session::new(Project::default());
    let (id, before) = saved_login_with_a_tile(&mut s, &server.base);
    let c = s.connections.connections.iter().find(|c| c.id == id).unwrap().clone();
    let read = crate::cmd::reads::read_ahead(&mut s, "project.importQgis", &p).unwrap();
    s.run("connection.login", &json!({"id": id, "apiKey": "RGSYNTH_typed_key", "allowInsecure": true})).unwrap();
    assert_ne!(s.map().tiles[0].url, before, "rebound by the user's login");
    drop(read);
    s.reclaim_rooms();
    assert!(crate::service_logins::logged_in(&s, &c));
    assert!(s.database_logins.session_password(&id).is_some());
}

/// The import makes a map of its own: another map made and shown while it reads leaves it to land
/// (#3120), as the user's own import.
#[test]
fn an_import_on_a_copy_lands_whichever_map_is_shown() {
    let dir = Folder::new("shown");
    let server = roads_server();
    let path = project(&dir, &server.base);
    let p = json!({"path": path.display().to_string()});
    let mut s = Session::new(Project::default());
    let read = crate::cmd::reads::read_ahead(&mut s, "project.importQgis", &p).unwrap();
    let done = read.run(&Control::new()).unwrap();
    s.run("map.new", &json!({"name": "Other"})).unwrap();
    assert_eq!(s.map().name, "Other");
    let answer = crate::cmd::reads::read_land(&mut s, done).unwrap();
    assert_eq!(answer["map"], "Ahead");
    assert_eq!(s.map().name, "Ahead");
    assert_eq!(s.project.maps.len(), 3);
}

/// A login two imports hold is the session's once one lands: the other one cancelled after leaves
/// it open, with the tiles the landing rebound to it (#3120).
#[test]
fn a_login_two_imports_hold_stays_open_when_one_lands_and_the_other_is_cancelled() {
    let dir = Folder::new("landed-one");
    let server = roads_server();
    let path = project(&dir, &server.base);
    let p = json!({"path": path.display().to_string()});
    let mut s = Session::new(Project::default());
    let (id, before) = saved_login_with_a_tile(&mut s, &server.base);
    let c = s.connections.connections.iter().find(|c| c.id == id).unwrap().clone();
    let first = crate::cmd::reads::read_ahead(&mut s, "project.importQgis", &p).unwrap();
    let second = crate::cmd::reads::read_ahead(&mut s, "project.importQgis", &p).unwrap();
    let done = first.run(&Control::new()).unwrap();
    crate::cmd::reads::read_land(&mut s, done).unwrap();
    let rebound = s.project.maps[0].tiles[0].url.clone();
    assert_ne!(rebound, before);
    drop(second);
    s.reclaim_rooms();
    assert!(crate::service_logins::logged_in(&s, &c), "the landed import's");
    assert_eq!(s.project.maps[0].tiles[0].url, rebound);
}

/// The desktop's way: the project read and parsed first ([`crate::cmd::reads::scan_import`]), then
/// the import started with it, which reads the file no more and makes the same map.
#[test]
fn an_import_scanned_first_makes_the_map_an_import_here_makes() {
    let dir = Folder::new("scanned");
    let server = roads_server();
    let path = project(&dir, &server.base);
    let p = json!({"path": path.display().to_string()});
    let mut here = Session::new(Project::default());
    let made = here.run("project.importQgis", &p).unwrap();
    let mut s = Session::new(Project::default());
    let scan = crate::cmd::reads::scan_import(&p, &Control::new(), false).unwrap();
    // Gone once read: the import reads it no more.
    std::fs::remove_file(&path).unwrap();
    let read = crate::cmd::reads::read_ahead_scanned(&mut s, &p, scan).unwrap();
    assert!(read.reads(), "the import runs on a copy");
    let done = read.run(&Control::new()).unwrap();
    let answer = crate::cmd::reads::read_land(&mut s, done).unwrap();
    assert_eq!(answer, made);
    assert!(super::cancellation::contents(&s.project) == super::cancellation::contents(&here.project));
    // Cancelled while it reads: no project, and nothing here.
    let control = Control::new();
    control.cancel();
    assert!(crate::cmd::reads::scan_import(&p, &control, false).is_err());
}

/// With the processing log on, the files the scan read (the project's) are the import's inputs,
/// as when it reads them itself; with it off, none is kept.
#[test]
fn an_import_scanned_first_logs_the_project_file_as_an_input() {
    let dir = Folder::new("logged");
    let server = roads_server();
    let path = project(&dir, &server.base);
    let p = json!({"path": path.display().to_string()});
    for on in [true, false] {
        let mut s = Session::new(Project::default());
        s.log.set_on(on);
        let scan = crate::cmd::reads::scan_import(&p, &Control::new(), on).unwrap();
        let read = crate::cmd::reads::read_ahead_scanned(&mut s, &p, scan).unwrap();
        let done = read.run(&Control::new()).unwrap();
        let logged = done.inputs.iter().any(|i| i.path.ends_with("ahead.qgs"));
        assert_eq!(logged, on, "{:?}", done.inputs.iter().map(|i| &i.path).collect::<Vec<_>>());
    }
}
