//! A publication's grid, scale bar and north arrow left out or set up by `layout.publication`
//! (#3094).

use super::*;
use rustgis_map::layout::{ArrowStyle, Element, ElementKind, GRID_LABEL_PT, Grid, GridKind, ScaleBarStyle, ScaleUnits};

/// An A4 sheet of open sea with `parts` added to the call: with nothing to cover, each surround
/// stays in its home corner.
fn sheet(parts: Value) -> (Session, Value) {
    let mut s = world();
    let r = publish(&mut s, parts).unwrap();
    (s, r)
}

fn publish(s: &mut Session, parts: Value) -> Result<Value, String> {
    let mut p = json!({"profile": "a4-portrait", "extent": [-30.0, -30.0, -20.0, -22.0]});
    p.as_object_mut().unwrap().extend(parts.as_object().unwrap().clone());
    s.run("layout.publication", &p)
}

fn find<'a>(s: &'a Session, name: &str) -> Option<&'a Element> {
    s.project.layouts[0].elements.iter().find(|e| e.name == name)
}

fn grid(s: &Session) -> Option<Grid> {
    match &find(s, "Map Frame").unwrap().kind {
        ElementKind::MapFrame { grid, .. } => grid.clone(),
        k => panic!("{k:?}"),
    }
}

/// Every text of the layout, its lines joined.
fn texts(s: &Session) -> String {
    let text = |e: &Element| match &e.kind {
        ElementKind::Text { text, .. } => Some(text.replace('\n', " ")),
        _ => None,
    };
    s.project.layouts[0].elements.iter().filter_map(text).collect::<Vec<_>>().join(" | ")
}

/// The room from the north arrow to the frame's right edge: the margin, and the grid's label strip.
fn right_room(s: &Session) -> f64 {
    let (frame, arrow) = (find(s, "Map Frame").unwrap(), find(s, "North Arrow").unwrap());
    frame.x + frame.w - (arrow.x + arrow.w)
}

/// The margin of an A4 sheet's surrounds from its frame's edge, in mm.
const MARGIN: f64 = 4.0;

#[test]
fn a_publication_given_empty_parts_is_the_default_one() {
    let elements = |parts: Value| serde_json::to_value(&sheet(parts).0.project.layouts[0].elements).unwrap();
    let default = elements(json!({}));
    assert_eq!(elements(json!({"grid": null, "scaleBar": null, "northArrow": null})), default);
    assert_eq!(elements(json!({"grid": {}, "scaleBar": {}, "northArrow": {}})), default);
    assert_eq!(elements(json!({"grid": {"step": null}, "scaleBar": {"textSize": null}, "northArrow": {"show": null}})), default);
}

#[test]
fn a_publication_without_a_grid_keeps_no_label_strips_and_names_no_graticule() {
    let (s, r) = sheet(json!({"grid": "none"}));
    assert_eq!(grid(&s), None);
    assert_eq!(r["grid"], Value::Null);
    assert!(!texts(&s).contains("Graticule") && texts(&s).contains("Datum:"), "{}", texts(&s));
    assert!((right_room(&s) - MARGIN).abs() < 1e-6, "{}", right_room(&s));
    let (default, _) = sheet(json!({}));
    assert!(right_room(&default) > MARGIN + 1.0);
}

#[test]
fn a_grid_token_typed_by_the_caller_says_a_dash_without_a_graticule() {
    let (s, _) = sheet(json!({"grid": "none", "reference": ["Lines every {grid}"]}));
    assert!(texts(&s).contains("Lines every —"), "{}", texts(&s));
}

#[test]
fn a_publication_grid_takes_its_step_and_the_reference_says_it() {
    let (s, r) = sheet(json!({"grid": {"step": 5}}));
    assert_eq!(grid(&s).unwrap().step, 5.0);
    assert_eq!(r["grid"], json!(5.0));
    assert!(texts(&s).contains("Graticule: 5°"), "{}", texts(&s));
}

#[test]
fn a_publication_measured_grid_picks_its_step_and_is_not_called_a_graticule() {
    let (s, r) = sheet(json!({"grid": {"type": "measured"}}));
    let g = grid(&s).unwrap();
    assert_eq!((g.kind, g.step), (GridKind::Measured, 0.0));
    assert_eq!(r["grid"], Value::Null);
    assert!(!texts(&s).contains("Graticule"), "{}", texts(&s));
}

#[test]
fn a_publication_grid_keeps_label_strips_only_where_it_labels() {
    let strip = right_room(&sheet(json!({})).0) - MARGIN;
    let (unlabelled, _) = sheet(json!({"grid": {"labels": false}}));
    assert!(grid(&unlabelled).is_some());
    assert!((right_room(&unlabelled) - MARGIN).abs() < 1e-6);
    let (bottom, _) = sheet(json!({"grid": {"edges": ["bottom"]}}));
    assert!((right_room(&bottom) - MARGIN).abs() < 1e-6);
    let (frame, bar) = (find(&bottom, "Map Frame").unwrap(), find(&bottom, "Scale").unwrap());
    assert!(frame.y + frame.h - (bar.y + bar.h) > MARGIN + 1.0);
    let (big, _) = sheet(json!({"grid": {"labelSize": 2.0 * GRID_LABEL_PT}}));
    assert!((right_room(&big) - MARGIN - 2.0 * strip).abs() < 1e-6, "{} {strip}", right_room(&big));
}

#[test]
fn a_publication_without_a_scale_bar_leaves_the_other_surrounds_in_place() {
    let (default, _) = sheet(json!({}));
    let (s, r) = sheet(json!({"scaleBar": "none"}));
    for name in ["Scale Box", "Scale Bar", "Scale"] {
        assert!(find(&s, name).is_none(), "{name}");
    }
    for name in ["North Arrow", "Legend Box"] {
        let at = |s: &Session| find(s, name).map(|e| (e.x, e.y, e.w, e.h));
        assert_eq!(at(&s), at(&default), "{name}");
    }
    assert!(!r["warnings"].to_string().contains("scale bar"), "{}", r["warnings"]);
}

#[test]
fn a_publication_scale_bar_takes_its_options_and_its_box_is_sized_for_it() {
    let (s, _) = sheet(json!({"scaleBar": {"units": "miles", "style": "line", "divisions": 3}}));
    let bar = find(&s, "Scale Bar").unwrap();
    assert!(
        matches!(bar.kind, ElementKind::ScaleBar { units: ScaleUnits::Miles, style: ScaleBarStyle::Line, divisions: 3, .. }),
        "{:?}",
        bar.kind
    );
    assert_eq!(find(&s, "Scale Box").unwrap().w, bar.w);
}

#[test]
fn a_publication_scale_bar_given_a_larger_text_gets_the_height_for_it() {
    let (default, _) = sheet(json!({}));
    let (s, _) = sheet(json!({"scaleBar": {"textSize": 14}}));
    let bar = find(&s, "Scale Bar").unwrap();
    assert!(matches!(bar.kind, ElementKind::ScaleBar { text_size, .. } if text_size == 14.0), "{:?}", bar.kind);
    assert!(bar.h > find(&default, "Scale Bar").unwrap().h);
    let said = s.notices_since(0);
    assert!(!said.iter().any(|n| n.text.starts_with("Scale bar")), "{said:?}");
}

#[test]
fn a_whole_globe_publication_has_no_scale_bar_whatever_its_options() {
    let mut s = world();
    s.run("map.projection", &json!({"type": "globe"})).unwrap();
    let r = s.run("layout.publication", &json!({"scaleBar": {"units": "miles"}})).unwrap();
    assert!(find(&s, "Scale Bar").is_none());
    assert!(r["warnings"].to_string().contains("no scale bar: the map shows the whole globe"), "{}", r["warnings"]);
    let r = s.run("layout.publication", &json!({"scaleBar": "none"})).unwrap();
    assert!(!r["warnings"].to_string().contains("no scale bar"), "{}", r["warnings"]);
}

#[test]
fn a_publication_north_arrow_takes_its_style_and_when_to_show() {
    let (s, _) = sheet(json!({"northArrow": {"style": "compass"}}));
    assert!(matches!(find(&s, "North Arrow").unwrap().kind, ElementKind::NorthArrow { style: ArrowStyle::Compass, .. }));
    // A continental span draws none unless asked "always".
    let wide = |arrow: Value| {
        let mut s = world();
        s.run("layout.publication", &json!({"extent": [-60.0, -30.0, -20.0, 0.0], "northArrow": arrow})).unwrap();
        find(&s, "North Arrow").map(|e| e.kind.clone())
    };
    assert_eq!(wide(json!({"style": "star"})), None);
    assert_eq!(wide(json!("auto")), None);
    assert!(wide(json!("always")).is_some());
    let shown = wide(json!({"show": "always", "style": "star"}));
    assert!(matches!(shown, Some(ElementKind::NorthArrow { style: ArrowStyle::Star, .. })), "{shown:?}");
    assert!(find(&sheet(json!({"northArrow": "none"})).0, "North Arrow").is_none());
}

#[test]
fn a_publication_part_given_wrong_is_refused_and_nothing_is_made() {
    for (parts, said) in [
        (json!({"grid": {"style": "wavy"}}), "unknown grid style"),
        (json!({"grid": {"foo": 1}}), "grid takes no \"foo\""),
        (json!({"grid": {"stepp": null}}), "grid takes no \"stepp\""),
        (json!({"grid": {"off": true}}), "grid takes no \"off\""),
        // A graticule draws degrees: it reads no coordinate system.
        // Keys only the other kind of grid reads.
        (json!({"grid": {"crs": "EPSG:32723"}}), "grid takes no \"crs\" for a graticule"),
        (json!({"grid": {"type": "graticule", "short": true}}), "grid takes no \"short\" for a graticule"),
        (json!({"grid": {"type": "measured", "format": "dms"}}), "grid takes no \"format\" for a measured grid"),
        (json!({"grid": {"type": "measured", "hemisphere": false}}), "grid takes no \"hemisphere\" for a measured grid"),
        (json!({"grid": 5}), "grid is \"none\""),
        (json!({"grid": true}), "grid is \"none\""),
        (json!({"scaleBar": {"units": "parsecs"}}), "unknown units"),
        (json!({"scaleBar": {"step": 1}}), "scaleBar takes no \"step\""),
        (json!({"scaleBar": "auto"}), "scaleBar is \"none\""),
        (json!({"northArrow": {"show": "none"}}), "northArrow's show"),
        (json!({"northArrow": {"style": "x"}}), "unknown north arrow style"),
        (json!({"northArrow": {"size": 2}}), "northArrow takes no \"size\""),
        (json!({"northArrow": "yes"}), "northArrow is \"auto\""),
    ] {
        let mut s = world();
        let maps = s.project.maps.len();
        let err = publish(&mut s, parts.clone()).unwrap_err();
        assert!(err.contains(said), "{parts}: {err}");
        assert_eq!((s.project.layouts.len(), s.project.maps.len()), (0, maps), "{parts}");
    }
}

#[test]
fn a_saved_publication_that_sets_a_part_requires_its_capability() {
    let requires = |params: Value| {
        let mut s = world();
        s.project
            .models
            .push(serde_json::from_value(json!({"name": "Sheet", "steps": [{"tool": "layout.publication", "params": params}]})).unwrap());
        serde_json::to_value(&s.project).unwrap()["requires"].to_string().contains("layout-publication-parts")
    };
    for params in [
        json!({"grid": "none"}),
        json!({"grid": {"step": 5}}),
        json!({"scaleBar": "none"}),
        json!({"scaleBar": {"units": "miles"}}),
        json!({"northArrow": {"style": "star"}}),
        // Any object: an older reader refuses one.
        json!({"northArrow": {"show": "auto"}}),
        json!({"northArrow": {}}),
        json!({"northArrow": {"show": null}}),
    ] {
        assert!(requires(params.clone()), "{params}");
    }
    for params in [json!({}), json!({"grid": null, "scaleBar": {}}), json!({"northArrow": "always"}), json!({"northArrow": null})] {
        assert!(!requires(params.clone()), "{params}");
    }
}

#[test]
fn the_publication_parts_fixture_opens_and_keeps_its_requirement() {
    let fixture = crate::Project::from_json(include_str!("../../../../schema/fixtures/layout_publication_parts.rgis")).unwrap();
    assert_eq!(serde_json::to_value(&fixture).unwrap()["requires"], json!(["layout-publication-parts"]));
}

#[test]
fn a_publication_scale_bar_too_large_for_the_sheet_says_it_does_not_fit() {
    let mut s = world();
    let mark = s.notice_mark();
    publish(&mut s, json!({"scaleBar": {"textSize": 100}})).unwrap();
    let said = s.notices_since(mark);
    assert_eq!(said.iter().filter(|n| n.text.starts_with("Scale bar \"Scale Bar\": its box is too small")).count(), 1, "{said:?}");
}

#[test]
fn a_publication_grid_says_what_its_typed_coordinate_system_loses() {
    let mut s = world();
    // UTM 17N on NAD 27, bound to WGS 84 by an NTv2 grid, which isn't applied.
    let crs = r#"BOUNDCRS[SOURCECRS[PROJCRS["unknown",BASEGEOGCRS["unknown",DATUM["North American Datum 1927",ELLIPSOID["Clarke 1866",6378206.4,294.978698213898,LENGTHUNIT["metre",1]],ID["EPSG",6267]],PRIMEM["Greenwich",0,ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",8901]]],CONVERSION["UTM zone 17N",METHOD["Transverse Mercator",ID["EPSG",9807]],PARAMETER["Latitude of natural origin",0,ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",8801]],PARAMETER["Longitude of natural origin",-81,ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",8802]],PARAMETER["Scale factor at natural origin",0.9996,SCALEUNIT["unity",1],ID["EPSG",8805]],PARAMETER["False easting",500000,LENGTHUNIT["metre",1],ID["EPSG",8806]],PARAMETER["False northing",0,LENGTHUNIT["metre",1],ID["EPSG",8807]],ID["EPSG",16017]],CS[Cartesian,2],AXIS["(E)",east,ORDER[1],LENGTHUNIT["metre",1,ID["EPSG",9001]]],AXIS["(N)",north,ORDER[2],LENGTHUNIT["metre",1,ID["EPSG",9001]]]]],TARGETCRS[GEOGCRS["WGS 84",DATUM["World Geodetic System 1984",ELLIPSOID["WGS 84",6378137,298.257223563,LENGTHUNIT["metre",1]]],PRIMEM["Greenwich",0,ANGLEUNIT["degree",0.0174532925199433]],CS[ellipsoidal,2],AXIS["latitude",north,ORDER[1],ANGLEUNIT["degree",0.0174532925199433]],AXIS["longitude",east,ORDER[2],ANGLEUNIT["degree",0.0174532925199433]],ID["EPSG",4326]]],ABRIDGEDTRANSFORMATION["unknown to WGS84",METHOD["NTv2",ID["EPSG",9615]],PARAMETERFILE["Latitude and longitude difference file","conus",ID["EPSG",8656]]]]"#;
    let mut said = |grid: Value| {
        let mark = s.notice_mark();
        publish(&mut s, json!({"grid": grid})).unwrap();
        s.notices_since(mark).iter().any(|n| n.text.starts_with("crs: "))
    };
    assert!(said(json!({"type": "measured", "crs": crs})));
    assert!(!said(json!({"type": "measured", "crs": "EPSG:32723"})));
}

/// The columns, title and layers left out of the layout's legend.
fn legend(s: &Session) -> (usize, String, Vec<String>) {
    match &find(s, "Legend").unwrap().kind {
        ElementKind::Legend { columns, title, exclude, .. } => (*columns, title.clone(), exclude.clone()),
        k => panic!("{k:?}"),
    }
}

/// The name of a sample layer the legend lists.
fn listed(s: &Session) -> String {
    s.map().layers.iter().find(|l| s.map().shows(l) && crate::render::layout::legend_lists(l)).unwrap().name.clone()
}

/// A path for a file of this test run.
fn temp(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("rustgis-3095-{}-{:?}-{name}", std::process::id(), std::thread::current().id()))
}

/// An A4 sheet of the sample map with a raster on a colour ramp over it, which gets a colorbar.
fn with_colorbar(parts: Value) -> (Session, Value) {
    let tif = temp("ramp.tif");
    std::fs::write(&tif, rustgis_data::raster::cog::example(256, 128)).unwrap();
    let mut s = world();
    s.run("layer.addData", &json!({"path": tif.display().to_string()})).unwrap();
    let r = publish(&mut s, parts).unwrap();
    let _ = std::fs::remove_file(tif);
    (s, r)
}

#[test]
fn a_publication_without_a_legend_has_no_legend_and_no_box() {
    let (s, r) = sheet(json!({"legend": "none", "colorbar": "none"}));
    assert!(find(&s, "Legend").is_none() && find(&s, "Legend Box").is_none());
    assert_eq!(r["legendColumns"], Value::Null);
    assert_eq!(r["legendBox"], json!(false));
    assert!(!r["warnings"].to_string().contains("legend"), "{}", r["warnings"]);
}

#[test]
fn a_publication_without_a_legend_keeps_the_colorbar_in_its_box() {
    let (s, r) = with_colorbar(json!({"legend": "none"}));
    assert!(find(&s, "Legend").is_none());
    let (bx, bar) = (find(&s, "Legend Box").unwrap(), find(&s, "Colorbar").unwrap());
    let (default, _) = with_colorbar(json!({}));
    assert!(bx.h < find(&default, "Legend Box").unwrap().h, "{}", bx.h);
    assert!(bar.y >= bx.y && bar.y + bar.h <= bx.y + bx.h + 1e-6);
    assert_eq!(r["legendColumns"], Value::Null);
}

#[test]
fn a_publication_legend_takes_its_columns() {
    for n in [1, 2, rustgis_map::layout::MAX_LEGEND_COLUMNS] {
        for style in ["classic", "panel"] {
            let (s, r) = sheet(json!({"legend": {"columns": n}, "style": style}));
            assert_eq!(legend(&s).0, n, "{style} {n}");
            assert_eq!(r["legendColumns"], json!(n), "{style} {n}");
        }
    }
}

#[test]
fn a_publication_legend_keeps_its_columns_on_a_veiled_map() {
    let mut s = Session::new(Project::default());
    let fill = [-30.0, 35.0, 70.0, 82.0];
    let [w, so, e, n] = fill;
    let ring = json!([[[w, so], [e, so], [e, n], [w, n], [w, so]]]);
    let fc = json!({"type": "FeatureCollection", "features": [{"type": "Feature", "properties": {}, "geometry": {"type": "Polygon", "coordinates": ring}}]});
    let path = temp("fill.geojson");
    std::fs::write(&path, fc.to_string()).unwrap();
    // Thirty layers make a legend too long for the room the map leaves, so the map is veiled.
    for _ in 0..30 {
        s.run("layer.addData", &json!({"path": path.display().to_string()})).unwrap();
    }
    let features: Vec<Value> = (0..)
        .map(|i| so + 0.25 * f64::from(i))
        .take_while(|lat| *lat <= 61.0)
        .map(|lat| json!({"type": "Feature", "properties": {}, "geometry": {"type": "Point", "coordinates": [20.0, lat]}}))
        .collect();
    let points = temp("points.geojson");
    std::fs::write(&points, json!({"type": "FeatureCollection", "features": features}).to_string()).unwrap();
    s.run("layer.addData", &json!({"path": points.display().to_string(), "stream": true, "name": "Column"})).unwrap();
    let r = s
        .run(
            "layout.publication",
            &json!({"profile": "a4-portrait", "extent": [10.0, 57.0, 30.0, 61.0], "author": "", "locator": "none", "legend": {"columns": 2}}),
        )
        .unwrap();
    for file in [path, points] {
        let _ = std::fs::remove_file(file);
    }
    assert_eq!(r["veiled"], json!(true), "{r}");
    assert_eq!((legend(&s).0, r["legendColumns"].as_u64()), (2, Some(2)), "{r}");
}

#[test]
fn a_publication_legend_leaves_out_the_layers_asked() {
    let name = listed(&world());
    let (s, _) = sheet(json!({"legend": {"exclude": [name.to_uppercase(), name.clone()]}}));
    let out = legend(&s).2;
    assert_eq!(out.iter().filter(|n| **n == name).count(), 1, "{out:?}");
    let (default, _) = sheet(json!({}));
    assert!(legend(&default).2.iter().all(|n| out.contains(n)), "{out:?}");
    assert!(find(&s, "Legend Box").unwrap().h < find(&default, "Legend Box").unwrap().h);
}

#[test]
fn a_publication_legend_title_is_drawn_and_the_box_grows_to_hold_it() {
    let (default, _) = sheet(json!({}));
    let (s, _) = sheet(json!({"legend": {"title": "Leyenda"}}));
    assert_eq!(legend(&s).1, "Leyenda");
    let (bx, was) = (find(&s, "Legend Box").unwrap(), find(&default, "Legend Box").unwrap());
    let title = crate::render::layout::legend_title_mm(rustgis_map::layout::LEGEND_TEXT_PT);
    assert!((bx.h - was.h - title).abs() < 1e-6, "{} {} {title}", bx.h, was.h);
    let long = "A legend title much wider than any of its rows";
    let (wide, _) = sheet(json!({"legend": {"title": long}}));
    let wanted = crate::render::layout::legend_title_width_mm(long, rustgis_map::layout::LEGEND_TEXT_PT);
    assert!(find(&wide, "Legend").unwrap().w >= wanted - 1e-6, "{wanted}");
}

#[test]
fn a_publication_legend_title_with_nothing_to_list_says_it_is_left_out() {
    let w = world();
    let all: Vec<&str> = w.map().layers.iter().map(|l| l.name.as_str()).collect();
    let (s, r) = sheet(json!({"legend": {"title": "Key", "exclude": all}}));
    assert!(find(&s, "Legend").is_none());
    assert!(r["warnings"].to_string().contains("the legend lists nothing"), "{}", r["warnings"]);
    let (_, r) = sheet(json!({"legend": {"exclude": all}}));
    assert!(!r["warnings"].to_string().contains("the legend lists nothing"), "{}", r["warnings"]);
}

#[test]
fn a_publication_legend_given_nothing_is_the_default_one() {
    let elements = |parts: Value| serde_json::to_value(&sheet(parts).0.project.layouts[0].elements).unwrap();
    let default = elements(json!({}));
    for legend in [
        json!(null),
        json!({}),
        json!({"title": null, "columns": null, "exclude": null}),
        json!({"title": "", "exclude": []}),
        json!({"title": "  "}),
    ] {
        assert_eq!(elements(json!({"legend": legend})), default, "{legend}");
    }
}

#[test]
fn a_publication_legend_given_wrong_is_refused_and_nothing_is_made() {
    let most = rustgis_map::layout::MAX_LEGEND_COLUMNS;
    let columns = format!("columns is a whole number from 1 to {most}");
    for (given, said) in [
        (json!({"columns": 0}), columns.as_str()),
        (json!({"columns": most + 1}), columns.as_str()),
        (json!({"columns": "2"}), columns.as_str()),
        (json!({"exclude": ["Nowhere"]}), "no layer 'Nowhere'"),
        (json!({"exclude": "Cities"}), "exclude is a list of layer names"),
        (json!({"exclude": [1]}), "exclude is a list of layer names"),
        (json!({"title": 5}), "title is text"),
        (json!({"title": "x".repeat(201)}), "title is 201 characters"),
        (json!({"labels": {}}), "legend takes no \"labels\" (it reads title, columns, exclude)"),
        (json!({"allClasses": true}), "legend takes no \"allClasses\""),
        (json!({"hideOther": true}), "legend takes no \"hideOther\""),
        (json!(5), "legend is \"none\""),
        (json!(true), "legend is \"none\""),
        (json!("auto"), "legend is \"none\""),
    ] {
        let mut s = world();
        let maps = s.project.maps.len();
        let err = publish(&mut s, json!({"legend": given})).unwrap_err();
        assert!(err.contains(said), "{given}: {err}");
        assert_eq!((s.project.layouts.len(), s.project.maps.len()), (0, maps), "{given}");
    }
}

#[test]
fn a_saved_publication_that_sets_its_legend_requires_its_capability() {
    let requires = |legend: Value| {
        let mut s = world();
        let step = json!({"tool": "layout.publication", "params": {"legend": legend}});
        s.project.models.push(serde_json::from_value(json!({"name": "Sheet", "steps": [step]})).unwrap());
        serde_json::to_value(&s.project).unwrap()["requires"].to_string().contains("layout-publication-legend")
    };
    for legend in [json!("none"), json!({"title": "Key"}), json!({"columns": 2}), json!({"exclude": ["Cities"]})] {
        assert!(requires(legend.clone()), "{legend}");
    }
    for legend in
        [json!(null), json!({}), json!({"title": null, "columns": null}), json!({"title": ""}), json!({"title": " ", "exclude": []})]
    {
        assert!(!requires(legend.clone()), "{legend}");
    }
}

#[test]
fn the_publication_legend_fixture_opens_and_keeps_its_requirement() {
    let fixture = crate::Project::from_json(include_str!("../../../../schema/fixtures/layout_publication_legend.rgis")).unwrap();
    assert_eq!(serde_json::to_value(&fixture).unwrap()["requires"], json!(["layout-publication-legend"]));
}

#[test]
fn a_publication_legend_title_wider_than_its_box_is_named() {
    let long = "A very long legend title ".repeat(8);
    for (columns, style) in [(1, "classic"), (2, "classic"), (1, "panel")] {
        let (_, r) = sheet(json!({"legend": {"title": long, "columns": columns}, "style": style}));
        let said = r["warnings"].to_string();
        assert!(said.contains("the legend's title is wider than its box: shorten it"), "{columns} {style}: {said}");
        assert!(!said.contains("columns are wider") && !said.contains("ends of long labels"), "{columns} {style}: {said}");
    }
    let (_, r) = sheet(json!({"legend": {"title": "Key"}}));
    assert!(!r["warnings"].to_string().contains("wider than its box"), "{}", r["warnings"]);
}

#[test]
fn a_publication_legend_does_not_wait_for_a_layer_it_leaves_out() {
    // A raster read in place from a host that never answers: still reading throughout.
    let host = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut s = Session::new(Project::default());
    let address = format!("http://{}/depth.tif", host.local_addr().unwrap());
    let mut raster = rustgis_map::raster::RasterLayer::opening_in_place(address);
    // Drawn by classes, so the legend would list it.
    raster.style.renderer = rustgis_map::raster::RasterRenderer::Colormap;
    s.map_mut().add_raster("Depth".to_string(), raster);
    let mut said = |legend: Value| {
        let p = json!({"profile": "a4-portrait", "extent": [0, 0, 10, 10], "colorbar": "none", "legend": legend});
        s.run("layout.publication", &p).unwrap()["warnings"].to_string()
    };
    let waited = said(json!({"title": "Key"}));
    assert!(waited.contains("classes aren't known yet"), "{waited}");
    let left_out = said(json!({"title": "Key", "exclude": ["Depth"]}));
    assert!(!left_out.contains("classes aren't known yet") && left_out.contains("the legend lists nothing"), "{left_out}");
}

#[test]
fn a_publication_legend_lists_the_layers_asked_to_leave_out_first() {
    // A loaded project keeps the first `MAX_LEGEND_EXCLUDED` names: those asked come first.
    let mut s = world();
    let name = "Cities".to_string();
    s.run("layout.publication", &json!({"extent": [0, 40, 20, 55], "legend": {"exclude": [name.clone()]}})).unwrap();
    let out = legend(&s).2;
    assert!(out.len() > 1, "a context layer too: {out:?}");
    assert_eq!(out[0], name);
}

#[test]
fn a_publication_without_a_legend_still_waits_for_a_layer_before_moving_the_map() {
    // Land drawn by classes filling the frame leaves the colorbar's box no free room, so the search
    // runs.
    let ring = json!([[[-30.0, -30.0], [40.0, -30.0], [40.0, 40.0], [-30.0, 40.0], [-30.0, -30.0]]]);
    let land = temp("land.geojson");
    let tif = temp("scale.tif");
    let fc = json!({"type": "FeatureCollection", "features": [{"type": "Feature", "properties": {"k": "a"}, "geometry": {"type": "Polygon", "coordinates": ring}}]});
    std::fs::write(&land, fc.to_string()).unwrap();
    std::fs::write(&tif, rustgis_data::raster::cog::example(256, 128)).unwrap();
    let mut s = Session::new(Project::default());
    for path in [&land, &tif] {
        s.run("layer.addData", &json!({"path": path.display().to_string()})).unwrap();
    }
    let name = s.map().layers.iter().find(|l| l.raster.is_none()).unwrap().name.clone();
    s.run("layer.symbology", &json!({"layer": name, "type": "unique", "field": "k"})).unwrap();
    // A raster read in place from a host that never answers: still reading throughout.
    let host = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}/depth.tif", host.local_addr().unwrap());
    let mut raster = rustgis_map::raster::RasterLayer::opening_in_place(address);
    raster.style.renderer = rustgis_map::raster::RasterRenderer::Colormap;
    s.map_mut().add_raster("Depth".to_string(), raster);
    // The land over the ramp, which keeps its colorbar from under it.
    s.run("layer.move", &json!({"layer": name, "to": 0})).unwrap();
    let p = json!({"profile": "a4-portrait", "extent": [0, 0, 20, 20], "author": "", "locator": "none", "legend": "none"});
    let r = s.run("layout.publication", &p).unwrap();
    for file in [land, tif] {
        let _ = std::fs::remove_file(file);
    }
    assert!(find(&s, "Colorbar").is_some(), "{r}");
    assert!(r["warnings"].to_string().contains("a layer there is still reading"), "{r}");
    assert_eq!((r["mapShift"][0].as_f64(), r["veiled"].as_bool()), (Some(0.0), Some(false)), "{r}");
}

#[test]
fn a_publication_legend_wider_than_its_box_is_said_under_a_wider_title_too() {
    // Six columns in a side column don't fit it, and the title is wider than they are.
    let long = "A very long legend title ".repeat(8);
    let (_, r) = sheet(json!({"legend": {"title": long, "columns": 6}, "style": "panel"}));
    let said = r["warnings"].to_string();
    assert!(said.contains("the legend's title is wider than its box"), "{said}");
    assert!(said.contains("the legend's columns are wider than its box"), "{said}");
}

#[test]
fn a_publication_legend_says_nothing_of_a_raster_it_does_not_key() {
    use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster, geotiff};
    // A colour table over more values than a legend can list.
    let mut s = Session::new(Project::default());
    let gt = GeoTransform { origin_x: 0.0, origin_y: 10.0, pixel_width: 0.5, pixel_height: -0.5 };
    let z: Vec<f32> = (0..400u16).map(f32::from).collect();
    let r = Raster::new("many", 20, 20, vec![z], gt, Crs::LonLat, None, ColorHint::Palette(vec![[0, 0, 0, 255]; 400]), "UInt16").unwrap();
    let bytes = geotiff::write(&r, 0).unwrap();
    let mut layer = rustgis_map::RasterLayer::from_bytes(r, "many.tif".to_string(), bytes);
    layer.style.renderer = rustgis_map::RasterRenderer::Colormap;
    crate::cmd::raster::add_raster(&mut s, "many", layer).unwrap();
    let mut said = |legend: Value| {
        let p = json!({"profile": "a4-portrait", "extent": [0, 0, 10, 10], "legend": legend});
        s.run("layout.publication", &p).unwrap()["warnings"].to_string()
    };
    let keyed = said(json!(null));
    assert!(keyed.contains("have no key in the legend: many"), "{keyed}");
    for legend in [json!("none"), json!({"exclude": ["many"]})] {
        let w = said(legend.clone());
        assert!(!w.contains("have no key in the legend"), "{legend}: {w}");
    }
}

/// The styles whose surrounds sit on cards, as the wizard is told them (#3172).
#[test]
fn only_modular_and_poster_put_their_surrounds_on_cards() {
    use crate::cmd::layout::{PUBLICATION_STYLES, publication_style_cards};
    let cards: Vec<&str> = PUBLICATION_STYLES.iter().map(|(id, _)| *id).filter(|id| publication_style_cards(id)).collect();
    assert_eq!(cards, ["modular", "poster"]);
    assert!(!publication_style_cards("unknown"));
}
