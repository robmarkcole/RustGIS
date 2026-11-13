//! Reprojection round trips, as a property: a longitude/latitude taken into a coordinate system
//! with `Transformer::from_lonlat` and back with `to_lonlat` is the place it was, within a
//! centimetre on the ground. Inside a system's area of use a place must go there and back; outside
//! it the system may refuse it (`None`), but a place it does take must come back, never somewhere
//! else. What a system does instead is listed in [`LIMITS`] and [`OUTLINE_REFUSALS`] with the
//! reason; a known bug is marked `bug:` (#1925). Places lean on the areas' edges and corners, the poles
//! and the antimeridian.
//!
//! Pull requests run a fixed seed; the nightly a new one with more cases. A failure gives its seed
//! and the place found that doesn't come back. To run a seed again, or deeper:
//! `PROPERTY_SEED=7 PROPERTY_CASES=200000 cargo test -p rustgis-data --test reprojection_round_trip`.
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestCaseError, TestRng, TestRunner};
use rustgis_data::raster::{Crs, Transformer};
use rustgis_geom::{Coord, wgs84_distance_m};
use std::sync::atomic::{AtomicUsize, Ordering};

/// How far (metres on the ground) a place may come back from where it was.
const TOLERANCE_M: f64 = 0.01;

/// Coordinate systems, each with its area of use (west, east, south, north; degrees, east of
/// 180° for one across the antimeridian), roughly as EPSG gives it.
const SYSTEMS: &[(&str, (f64, f64, f64, f64))] = &[
    ("EPSG:3857", (-180.0, 180.0, -85.051_128, 85.051_128)),
    ("EPSG:32617", (-84.0, -78.0, 0.0, 84.0)),
    ("EPSG:32760", (174.0, 180.0, -80.0, 0.0)),
    ("EPSG:32601", (-180.0, -174.0, 0.0, 84.0)),
    ("EPSG:32661", (-180.0, 180.0, 60.0, 90.0)),
    ("EPSG:32761", (-180.0, 180.0, -90.0, -60.0)),
    ("EPSG:3413", (-180.0, 180.0, 30.0, 90.0)),
    ("EPSG:3031", (-180.0, 180.0, -90.0, -60.0)),
    ("EPSG:2154", (-9.86, 10.38, 41.15, 51.56)),
    ("EPSG:3347", (-141.0, -47.0, 38.0, 86.0)),
    ("EPSG:5070", (-124.79, -66.91, 24.41, 49.38)),
    ("EPSG:3577", (112.0, 154.0, -44.0, -10.0)),
    ("EPSG:3035", (-35.58, 44.83, 24.6, 84.73)),
    ("EPSG:3338", (-172.54, -129.99, 51.3, 71.4)),
    ("EPSG:27700", (-9.0, 2.0, 49.75, 61.0)),
    ("EPSG:2056", (5.96, 10.49, 45.82, 47.81)),
    ("EPSG:2193", (166.37, 178.63, -47.33, -34.1)),
    ("EPSG:3832", (98.0, 300.0, -60.0, 66.67)),
    ("EPSG:4087", (-180.0, 180.0, -90.0, 90.0)),
    ("EPSG:6933", (-180.0, 180.0, -86.0, 86.0)),
    ("+proj=sinu +lon_0=0 +x_0=0 +y_0=0 +R=6371007.181 +units=m +no_defs", (-180.0, 180.0, -90.0, 90.0)),
    ("+proj=robin +lon_0=0 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs", (-180.0, 180.0, -90.0, 90.0)),
    ("+proj=natearth +lon_0=0 +datum=WGS84 +units=m +no_defs", (-180.0, 180.0, -90.0, 90.0)),
    ("+proj=wintri +lon_0=0 +datum=WGS84 +units=m +no_defs", (-180.0, 180.0, -90.0, 90.0)),
    ("+proj=eck4 +lon_0=0 +datum=WGS84 +units=m +no_defs", (-180.0, 180.0, -90.0, 90.0)),
];

/// Where a system doesn't bring a place back to within [`TOLERANCE_M`], as (system, how far in
/// metres a place may come back from where it was, whether inside its area of use too or only
/// outside it, why). Each is PROJ's behaviour too, checked with PROJ 9.8.1.
const LIMITS: &[(&str, f64, bool, &str)] = &[
    ("EPSG:3857", f64::INFINITY, false, "a latitude past 85.05° is taken as 85.05°, where Web Mercator ends; an export says so"),
    // bug: transverse Mercator takes places up to 150° from its central meridian (as PROJ does),
    // and those near its singular points, 90° off on the equator, come back anywhere.
    ("EPSG:32617", f64::INFINITY, false, "transverse Mercator takes places it can't bring back"),
    ("EPSG:32760", f64::INFINITY, false, "transverse Mercator takes places it can't bring back"),
    ("EPSG:32601", f64::INFINITY, false, "transverse Mercator takes places it can't bring back"),
    ("EPSG:2193", f64::INFINITY, false, "transverse Mercator takes places it can't bring back"),
    ("EPSG:27700", f64::INFINITY, false, "transverse Mercator takes places it can't bring back"),
    // The authalic latitude's sine is nearly 1 near a pole, and its arcsine loses half its digits
    // there: about R·√ε, a decimetre.
    ("EPSG:3035", 0.2, false, "the authalic latitude rounds within a ten-thousandth of a degree of a pole"),
    ("EPSG:6933", 0.2, false, "the authalic latitude rounds within a ten-thousandth of a degree of a pole"),
    // Eckert IV: within a millionth of a degree of a pole, y is a rounding step or two from the
    // pole line's, which is about a centimetre on the ground (`rustgis_geom::world`, #1911).
    ("+proj=eck4 +lon_0=0 +datum=WGS84 +units=m +no_defs", 0.05, true, "a Double holds y to about a centimetre near a pole"),
    // Robinson's curve between its table rows (every 5°) is in pieces whose single-precision
    // coefficients don't quite meet: at 10°, 35°, 45°, 65° and 90° a band of y up to 2.13 m wide is
    // reached from both sides of the row, and the inverse takes the side above. As PROJ's.
    ("+proj=robin +lon_0=0 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs", 2.5, true, "Robinson's pieces overlap at some table rows"),
    // bug: Albers' inverse (proj4rs's, as PROJ's) snaps a latitude within about 0.02° of a pole
    // onto it.
    ("EPSG:5070", 5_000.0, false, "Albers takes a place near a pole as the pole"),
    ("EPSG:3577", 5_000.0, false, "Albers takes a place near a pole as the pole"),
    ("EPSG:3338", 5_000.0, false, "Albers takes a place near a pole as the pole"),
    // bug: oblique Mercator takes a place more than 90° round its sphere from Bern (as PROJ
    // does), and brings it back mirrored, thousands of kilometres away.
    ("EPSG:2056", f64::INFINITY, false, "Swiss oblique Mercator mirrors the far side of the globe"),
];

/// Systems that refuse a place in their area of use on a world map's outline (within a hundredth
/// of a degree of ±180° or a pole), as (system, why).
const OUTLINE_REFUSALS: &[(&str, &str)] = &[
    // bug: Robinson's inverse refuses a longitude its latitude's rounding puts past ±180°, so a
    // world Robinson file loses every shape that touches the map's edge.
    ("+proj=robin +lon_0=0 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs", "Robinson's inverse refuses the map's edge"),
];

/// A number from the environment, else `default`.
fn env(name: &str, default: u64) -> u64 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// A longitude folded into −180..180.
fn fold(lon: f64) -> f64 {
    if lon > 180.0 { lon - 360.0 } else { lon }
}

/// A place in an area of use, often on its edges or corners: (place, whether it is inside).
fn inside((w, e, s, n): (f64, f64, f64, f64)) -> impl Strategy<Value = (Coord, bool)> {
    let lon = prop_oneof![3 => w..=e, 1 => prop::sample::select(vec![w, e, (w + e) / 2.0])];
    let lat = prop_oneof![3 => s..=n, 1 => prop::sample::select(vec![s, n, (s + n) / 2.0])];
    (lon, lat).prop_map(|(x, y)| (Coord::new(fold(x), y), true))
}

/// A place anywhere, often on an edge of the globe: ±180°, the poles, Web Mercator's limit.
fn anywhere() -> impl Strategy<Value = (Coord, bool)> {
    let lon = prop_oneof![3 => -180.0..=180.0f64, 1 => prop::sample::select(vec![-180.0, 180.0, 0.0, 179.999_999, -90.0, 90.0])];
    let lat = prop_oneof![3 => -90.0..=90.0f64, 1 => prop::sample::select(vec![-90.0, 90.0, 0.0, 85.06, -85.06, 89.999_999])];
    (lon, lat).prop_map(|(x, y)| (Coord::new(x, y), false))
}

/// A system (its index in [`SYSTEMS`]) and a place, inside its area of use or anywhere.
fn case() -> impl Strategy<Value = (usize, Coord, bool)> {
    (0..SYSTEMS.len()).prop_flat_map(|i| {
        let area = SYSTEMS.get(i).map_or((-180.0, 180.0, -90.0, 90.0), |s| s.1);
        prop_oneof![3 => inside(area), 1 => anywhere()].prop_map(move |(c, ins)| (i, c, ins))
    })
}

/// Whether `p` is in the area of use `(w, e, s, n)`, whose east edge may be past 180°.
fn in_area((w, e, s, n): (f64, f64, f64, f64), p: Coord) -> bool {
    (s..=n).contains(&p.y) && [p.x - 360.0, p.x, p.x + 360.0].iter().any(|x| (w..=e).contains(x))
}

#[test]
fn a_place_on_the_antimeridian_is_in_an_area_on_either_side() {
    let zone_1 = (-180.0, -174.0, 0.0, 84.0);
    assert!(in_area(zone_1, Coord::new(180.0, 45.0)) && in_area(zone_1, Coord::new(-180.0, 45.0)));
    assert!(in_area((174.0, 180.0, -80.0, 0.0), Coord::new(-180.0, -45.0)));
    assert!(!in_area(zone_1, Coord::new(179.0, 45.0)));
}

/// Cases that went there and back, and places that were refused.
static TRIPS: AtomicUsize = AtomicUsize::new(0);
static REFUSED: AtomicUsize = AtomicUsize::new(0);

/// Takes a place there and back: how far (metres) it came back from where it was, or `None` when
/// it was refused.
fn trip(t: &Transformer, p: Coord) -> Result<Option<f64>, String> {
    let Some(q) = t.from_lonlat(p) else { return Ok(None) };
    let Some(back) = t.to_lonlat(q) else { return Err(format!("{p:?} went to {q:?}, which didn't come back")) };
    let off = wgs84_distance_m(p, back);
    if off.is_nan() {
        return Err(format!("{p:?} went to {q:?} and came back at {back:?}"));
    }
    Ok(Some(off))
}

#[test]
fn a_place_taken_into_a_coordinate_system_comes_back() {
    let transformers: Vec<Transformer> = SYSTEMS
        .iter()
        .map(|(text, _)| Crs::parse(text).and_then(|c| c.transformer()).map_err(|e| format!("{text}: {e}")).unwrap())
        .collect();
    let seed = env("PROPERTY_SEED", 1);
    let mut bytes = [0u8; 32];
    bytes[..8].copy_from_slice(&seed.to_le_bytes());
    let cases = u32::try_from(env("PROPERTY_CASES", 20_000)).unwrap_or(20_000);
    let config = Config { cases, failure_persistence: None, ..Config::default() };
    let mut runner = TestRunner::new_with_rng(config, TestRng::from_seed(RngAlgorithm::ChaCha, &bytes));
    let result = runner.run(&case(), |(i, p, _)| {
        let (Some((name, area)), Some(t)) = (SYSTEMS.get(i), transformers.get(i)) else { return Ok(()) };
        // Inside the area of use by where the place is, whichever way it was made.
        let inside = in_area(*area, p);
        let limit = LIMITS.iter().find(|(s, _, anywhere, _)| s == name && (*anywhere || !inside));
        let allowed = limit.map_or(TOLERANCE_M, |l| l.1.max(TOLERANCE_M));
        let edge = (p.x.abs() > 179.99 || p.y.abs() > 89.99) && OUTLINE_REFUSALS.iter().any(|(s, _)| s == name);
        let trip = match trip(t, p) {
            // Only a refusal on the way back, not a place that comes back wrong.
            Err(e) if edge && e.contains("didn't come back") => None,
            other => other.map_err(|e| TestCaseError::fail(format!("{name}: {e}")))?,
        };
        match trip {
            None if inside && !edge => Err(TestCaseError::fail(format!("{name}: {p:?} is in the area of use but was refused"))),
            None => {
                REFUSED.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Some(off) if off > allowed => Err(TestCaseError::fail(format!("{name}: {p:?} came back {off:.3e} m away"))),
            Some(_) => {
                TRIPS.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
        }
    });
    eprintln!("{} places there and back, {} refused", TRIPS.load(Ordering::Relaxed), REFUSED.load(Ordering::Relaxed));
    if let Err(e) = result {
        panic!("PROPERTY_SEED={seed}: {e}");
    }
}
