//! WGS 84 Universal Polar Stereographic (UPS) coordinates: the grid of the polar areas, north of
//! 84° N and south of 80° S, where the UTM zones stop, and the 100 km squares MGRS uses there.
//!
//! The grid follows NGA.SIG.0012 (2014-03-25), "The Universal Grids and the Transverse Mercator
//! and Polar Stereographic Map Projections":
//!
//! - polar stereographic on WGS 84, with a scale of exactly 0.994 at the pole and a false easting
//!   and false northing of 2,000,000 m (§10.1);
//! - the projection of eqs. (18)–(20) in §8, placed as eqs. (21) and (22) in §9.1 say: in both
//!   zones the easting grows toward 90° E, and the northing grows toward 180° in the north and
//!   toward 0° in the south, so the 2,000,000 m easting runs along the 0° and 180° meridians
//!   (DMA TM 8358.2, §3-2.4);
//! - north UPS for latitudes of 84° N and more, south UPS for latitudes south of 80° S (§10.4):
//!   84° N itself is UPS, 80° S itself is UTM;
//! - MGRS in the polar areas (§§11.9–11.13, and NGA.STND.0037 §3-2c): no zone number, a letter for
//!   the half (Y for western longitudes and Z for eastern ones in the north, A and B in the
//!   south; the 0° and 180° meridians, at an easting of 2,000,000 m, are Z and B, by §§11.10 and
//!   11.11, where square 20 is ZA and BA), then the 100 km square's column and row letters, and
//!   the digits truncated to the precision.
//!
//! The conformal latitude is the one in [`crate::utm`] (Karney 2011, eqs. 7–9 and 19–21), which
//! gives the same values as §2.8 and §2.9's PhiToChi and ChiToPhi.

use std::fmt;
use std::sync::LazyLock;

use crate::Coord;
use crate::utm::{
    GridError, MAX_REF_CHARS, ROUNDING_DEG, WGS84_A, conformal_tan, easting_and_northing, eccentricity, geographic_tan, wrap,
};

/// The northern limit of the UTM zones, where north UPS begins (degrees); 84° N itself is UPS.
pub const UPS_NORTH_FROM: f64 = crate::utm::UTM_MAX_LAT;
/// The southern limit of the UTM zones (degrees); south UPS is south of it, 80° S itself is UTM.
pub const UPS_SOUTH_OF: f64 = crate::utm::UTM_MIN_LAT;

/// UPS's scale at the pole, exactly (NGA.SIG.0012 §10.1).
const K0: f64 = 0.994;
/// The false easting and false northing: the pole's grid coordinates (meters).
const FALSE_ORIGIN: f64 = 2_000_000.0;
/// MGRS 100 km square side (meters).
const SQUARE: f64 = 100_000.0;
/// The farthest from the pole (meters) an easting and northing are converted. Every distance maps
/// to some latitude (about 2° on the zone's side of the equator at 12,000 km, 25° beyond it at
/// 20,000 km); past this the numbers can only be typing errors, so they are refused.
const MAX_RADIUS: f64 = 20_000_000.0;

/// k90 of NGA.SIG.0012 eq. (19): √(1 − e²) · exp(e · artanh e).
static K90: LazyLock<f64> = LazyLock::new(|| {
    let e = eccentricity();
    (1.0 - e * e).sqrt() * (e * e.atanh()).exp()
});

/// The north zone's 100 km square column letters (band, column) from an easting of 1,300,000 m,
/// and its row letters from a northing of 1,300,000 m (NGA.SIG.0012 §11.10).
const NORTH_COLUMNS: [&[u8; 2]; 14] = [b"YR", b"YS", b"YT", b"YU", b"YX", b"YY", b"YZ", b"ZA", b"ZB", b"ZC", b"ZF", b"ZG", b"ZH", b"ZJ"];
const NORTH_ROWS: &[u8; 14] = b"ABCDEFGHJKLMNP";
/// The first 100 km square of the north zone's lettering, along either axis.
const NORTH_FIRST: i64 = 13;
/// The south zone's column letters from an easting of 800,000 m, and its row letters from a
/// northing of 800,000 m (NGA.SIG.0012 §11.11).
const SOUTH_COLUMNS: [&[u8; 2]; 24] = [
    b"AJ", b"AK", b"AL", b"AP", b"AQ", b"AR", b"AS", b"AT", b"AU", b"AX", b"AY", b"AZ", b"BA", b"BB", b"BC", b"BF", b"BG", b"BH", b"BJ",
    b"BK", b"BL", b"BP", b"BQ", b"BR",
];
const SOUTH_ROWS: &[u8; 24] = b"ABCDEFGHJKLMNPQRSTUVWXYZ";
/// The first 100 km square of the south zone's lettering, along either axis.
const SOUTH_FIRST: i64 = 8;

/// sin and cos of an angle in degrees, exact at multiples of 90°: on the 0° and 180° meridians the
/// easting is exactly 2,000,000 m, so a place there is in the Z or B half, as MGRS requires, and
/// longitude −180° falls on the same side as 180°.
fn sincosd(deg: f64) -> (f64, f64) {
    let q = (deg / 90.0).round();
    let (s, c) = (deg - 90.0 * q).to_radians().sin_cos();
    match (q as i64).rem_euclid(4) {
        0 => (s, c),
        1 => (c, -s),
        2 => (-s, -c),
        _ => (-c, s),
    }
}

/// The distance on the grid from the zone's own pole to a latitude measured toward that pole, in
/// meters: 2 a k0 / k90 · cos χ / (1 + sin χ), χ the conformal latitude (NGA.SIG.0012 eqs. 18 and
/// 21). cos χ / (1 + sin χ) is 1 / (τ′ + √(1 + τ′²)), written as √(1 + τ′²) − τ′ when τ′ is
/// negative so that it doesn't cancel. `None` at the other pole, where the projection has no
/// place.
fn polar_radius(lat: f64) -> Option<f64> {
    if lat >= 90.0 {
        return Some(0.0);
    }
    if lat <= -90.0 {
        return None;
    }
    let taup = conformal_tan(lat.to_radians().tan());
    let h = taup.hypot(1.0);
    let t = if taup >= 0.0 { 1.0 / (h + taup) } else { h - taup };
    let rho = 2.0 * WGS84_A * K0 / *K90 * t;
    rho.is_finite().then_some(rho)
}

/// The latitude, measured toward the zone's own pole, at a distance from that pole on the grid:
/// with r = ρ k90 / (2 a k0), tan χ = (1 − r²) / 2r (NGA.SIG.0012 eq. 20), then the latitude from
/// the conformal one.
fn latitude_at(rho: f64) -> Option<f64> {
    if rho == 0.0 {
        return Some(90.0);
    }
    let r = rho * *K90 / (2.0 * WGS84_A * K0);
    let lat = geographic_tan((1.0 - r * r) / (2.0 * r))?.atan().to_degrees();
    lat.is_finite().then_some(lat)
}

/// Which UPS zone a latitude lies in: `Some(true)` for north UPS (84° N to the pole),
/// `Some(false)` for south UPS (south of 80° S), `None` in the UTM latitudes, past a pole or for
/// a non-finite latitude.
pub fn ups_zone(lat: f64) -> Option<bool> {
    if (UPS_NORTH_FROM..=90.0).contains(&lat) {
        Some(true)
    } else if (-90.0..UPS_SOUTH_OF).contains(&lat) {
        Some(false)
    } else {
        None
    }
}

/// A place in UPS: the zone (north or south) and the easting and northing in meters; the pole is
/// at 2,000,000 m, 2,000,000 m.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ups {
    pub north: bool,
    pub easting: f64,
    pub northing: f64,
}

impl Ups {
    /// The zone spelled out, as the readout writes it: `north` or `south`.
    pub fn hemisphere(&self) -> &'static str {
        if self.north { "north" } else { "south" }
    }
}

impl fmt::Display for Ups {
    /// `north 2392768 1607232`, `south 2096454 1452981`: the zone, then the easting and northing
    /// rounded to the meter, as GeographicLib's `GeoConvert -u -l` writes UPS (UPS has no zone
    /// number).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {:.0} {:.0}", self.hemisphere(), self.easting.round(), self.northing.round())
    }
}

/// A place's UPS coordinates; `None` in the UTM latitudes (80° S up to 84° N) and for a non-finite
/// place.
pub fn to_ups(c: Coord) -> Option<Ups> {
    // ups_zone refuses a non-finite latitude, and to_ups_in_zone a non-finite longitude.
    let north = ups_zone(c.y)?;
    let (easting, northing) = to_ups_in_zone(c, north)?;
    Some(Ups { north, easting, northing })
}

/// A place's easting and northing in a UPS zone, as a projection to that zone works (EPSG:5041
/// north, 5042 south), at any latitude but the other pole. `None` for a non-finite place, a
/// latitude past ±90° or the other pole.
pub fn to_ups_in_zone(c: Coord, north: bool) -> Option<(f64, f64)> {
    if !c.is_finite() || c.y.abs() > 90.0 {
        return None;
    }
    let rho = polar_radius(if north { c.y } else { -c.y })?;
    let (sin, cos) = sincosd(wrap(c.x));
    let x = FALSE_ORIGIN + rho * sin;
    let y = if north { FALSE_ORIGIN - rho * cos } else { FALSE_ORIGIN + rho * cos };
    (x.is_finite() && y.is_finite()).then_some((x, y))
}

/// The place (longitude, latitude) at an easting and northing in a UPS zone. The pole itself gets
/// longitude 0°. `None` for non-finite values, or an easting and northing more than 20,000 km from
/// the pole, which name no place.
pub fn from_ups(north: bool, easting: f64, northing: f64) -> Option<Coord> {
    if !easting.is_finite() || !northing.is_finite() {
        return None;
    }
    let (dx, dy) = (easting - FALSE_ORIGIN, northing - FALSE_ORIGIN);
    let rho = dx.hypot(dy);
    if rho > MAX_RADIUS {
        return None;
    }
    let lon = if rho == 0.0 {
        0.0
    } else if north {
        dx.atan2(-dy).to_degrees()
    } else {
        dx.atan2(dy).to_degrees()
    };
    let lat = latitude_at(rho)?;
    Some(Coord::new(wrap(lon), if north { lat } else { -lat }))
}

/// A place as `north 2392768 1607232` (see [`Ups`]'s `Display`); `None` in the UTM latitudes.
pub fn format_ups(c: Coord) -> Option<String> {
    to_ups(c).map(|u| u.to_string())
}

/// A zone's lettering: its column letter pairs, its row letters, and the index of its first
/// 100 km square along either axis (NGA.SIG.0012 §§11.10 and 11.11).
fn lettering(north: bool) -> (&'static [&'static [u8; 2]], &'static [u8], i64) {
    if north { (&NORTH_COLUMNS, NORTH_ROWS, NORTH_FIRST) } else { (&SOUTH_COLUMNS, SOUTH_ROWS, SOUTH_FIRST) }
}

/// The MGRS letters of the 100 km square holding a UPS easting and northing (whole meters): the
/// half's letter (Y, Z, A or B), then the square's column and row letters. `None` outside the
/// lettered area of the zone (NGA.SIG.0012 §§11.10 and 11.11).
pub(crate) fn square_letters(north: bool, easting: i64, northing: i64) -> Option<(char, char, char)> {
    let (col, row) = (easting.div_euclid(100_000), northing.div_euclid(100_000));
    let (columns, rows, first) = lettering(north);
    let pair = columns.get(usize::try_from(col - first).ok()?)?;
    let row = rows.get(usize::try_from(row - first).ok()?)?;
    Some((char::from(pair[0]), char::from(pair[1]), char::from(*row)))
}

/// The zone and the south-west corner (easting, northing in meters) of the 100 km square that a
/// half's letter and the square's letters name. [`GridError::Band`] for a letter that isn't a
/// polar half, [`GridError::Square`] for square letters that half doesn't use.
pub(crate) fn square_origin(half: char, column: char, row: char) -> Result<(bool, f64, f64), GridError> {
    let north = match half {
        'Y' | 'Z' => true,
        'A' | 'B' => false,
        _ => return Err(GridError::Band),
    };
    let (columns, rows, first) = lettering(north);
    let col = columns.iter().position(|p| char::from(p[0]) == half && char::from(p[1]) == column).ok_or(GridError::Square)?;
    let row = rows.iter().position(|r| char::from(*r) == row).ok_or(GridError::Square)?;
    let at = |i: usize| (first + i as i64) as f64 * SQUARE;
    Ok((north, at(col), at(row)))
}

/// Whether a UPS cell (south-west corner `e0`, `n0`, side `cell`) reaches into its zone: the
/// point of the cell nearest the pole lies at 84° N or more in the north, south of 80° S in the
/// south, give or take `slop_deg` for rounding. Squares at the corners of a zone's lettering lie
/// wholly in the UTM latitudes, and a reference there names no UPS place.
pub(crate) fn cell_reaches_zone(north: bool, e0: f64, n0: f64, cell: f64, slop_deg: f64) -> bool {
    let near_pole = |lo: f64| FALSE_ORIGIN.clamp(lo, lo + cell);
    from_ups(north, near_pole(e0), near_pole(n0)).is_some_and(|p| in_zone(north, p.y, slop_deg))
}

/// Whether a latitude lies in a UPS zone (84° N or more, or south of 80° S), give or take
/// `slop_deg`.
fn in_zone(north: bool, lat: f64, slop_deg: f64) -> bool {
    if north { lat >= UPS_NORTH_FROM - slop_deg } else { lat < UPS_SOUTH_OF + slop_deg }
}

/// Whether a typed easting lies in a polar half's side of its zone: Y and A west of the
/// 2,000,000 m easting, Z and B on it and east of it (NGA.STND.0037 App. B-5b; the line itself
/// is Z and B by NGA.SIG.0012 §§11.10 and 11.11, where square 20 is ZA and BA). Full
/// coordinates are written rounded to the meter (NGA.STND.0037 App. A-2), so a place in Y or A
/// just west of the line may read 2,000,000: Y and A accept exactly that whole number too.
pub(crate) fn easting_in_half(half: char, easting: f64) -> bool {
    match half {
        'Y' | 'A' => easting <= FALSE_ORIGIN,
        'Z' | 'B' => easting >= FALSE_ORIGIN,
        _ => false,
    }
}

/// Whether an easting and northing lie in their zone's lettered area, where every UPS position
/// in the zone falls: 1,300,000 m up to 2,700,000 m in the north, 800,000 m up to 3,200,000 m in
/// the south, along both axes (NGA.SIG.0012 §§11.10 and 11.11). Numbers outside it aren't a UPS
/// position at all.
pub(crate) fn in_lettered_area(north: bool, easting: f64, northing: f64) -> bool {
    let first = lettering(north).2 as f64 * SQUARE;
    let span = first..(2.0 * FALSE_ORIGIN - first);
    span.contains(&easting) && span.contains(&northing)
}

/// Whether a letter names a polar half of MGRS.
pub(crate) fn is_polar_half(letter: char) -> bool {
    matches!(letter, 'A' | 'B' | 'Y' | 'Z')
}

/// Reads a UPS position, which has no zone number: the zone spelled out, `north 2392768 1607232`
/// or `south 2096454 1452981` (any case), as the readout writes it, or as GeographicLib's short
/// form, `n 2392768 1607232`; or the polar half's letter before the easting and northing,
/// `Y, 1735147, 2243078` (NGA.STND.0037 App. A-4), where Y and A lie west of the 2,000,000 m
/// easting (or on it, for a rounded coordinate) and Z and B on or east of it. Numbers outside the
/// zone's lettered area (north 1,300,000 up to 2,700,000 m, south 800,000 up to 3,200,000 m)
/// aren't a UPS position and read as text ([`GridError::Syntax`]): `A 10 20` is a road, and
/// `north 412345 5523456` a UTM position without its zone. A place inside it
/// must lie in the zone, at 84° N or more or south of 80° S, give or take the meter it was rounded
/// to.
pub fn parse_ups(text: &str) -> Result<Coord, GridError> {
    let text = text.trim();
    if text.chars().count() > MAX_REF_CHARS {
        return Err(GridError::Syntax);
    }
    let mut chars = text.chars().peekable();
    let mut word = String::new();
    while let Some(c) = chars.next_if(char::is_ascii_alphabetic) {
        word.push(c.to_ascii_uppercase());
    }
    let (north, half) = match word.as_str() {
        "NORTH" | "N" => (true, None),
        "SOUTH" | "S" => (false, None),
        "Y" | "Z" => (true, word.chars().next()),
        "A" | "B" => (false, word.chars().next()),
        _ => return Err(GridError::Syntax),
    };
    // A space or comma after the word or letter: `north2000000 …` isn't written that way.
    if !chars.peek().is_some_and(|c| c.is_whitespace() || *c == ',') {
        return Err(GridError::Syntax);
    }
    let rest: String = chars.collect();
    let (easting, northing) = easting_and_northing(&rest)?;
    if !in_lettered_area(north, easting, northing) {
        // Not a UPS position: a road or a code (`A 10 20`), or a UTM position without its zone
        // (`north 412345 5523456`), which "outside the zone" would mislead about. Text.
        return Err(GridError::Syntax);
    }
    if half.is_some_and(|h| !easting_in_half(h, easting)) {
        return Err(GridError::OutsideZone);
    }
    let p = from_ups(north, easting, northing).ok_or(GridError::OutsideZone)?;
    if !in_zone(north, p.y, ROUNDING_DEG) {
        return Err(GridError::OutsideZone);
    }
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(lon: f64, lat: f64) -> Coord {
        Coord::new(lon, lat)
    }

    /// NGA.SIG.0012 §10.2 (north UPS, WGS 84): longitude, latitude → easting, northing, printed to
    /// the micrometer.
    #[test]
    fn forward_matches_nga_table() {
        let rows = [
            (90.0, 90.0, 2_000_000.000_000, 2_000_000.000_000),
            (-179.0, 89.0, 1_998_062.320_046, 2_111_009.610_243),
            (-90.0, 88.0, 1_777_930.731_071, 2_000_000.000_000),
            (-1.0, 87.0, 1_994_185.827_038, 1_666_906.254_073),
            (0.0, 86.0, 2_000_000.000_000, 1_555_731.570_643),
            (1.0, 85.0, 2_009_694.068_153, 1_444_627.207_468),
            (89.0, 84.0, 2_666_626.157_825, 1_988_363.997_132),
            (90.0, 83.0, 2_778_095.750_322, 2_000_000.000_000),
            (91.0, 82.0, 2_889_442.490_749, 2_015_525.276_426),
            (179.0, 81.0, 2_017_473.190_606, 3_001_038.419_357),
            (180.0, 80.0, 2_000_000.000_000, 3_112_951.136_955),
            (0.0, 40.0, 2_000_000.000_000, -3_918_313.984_953),
            (-90.0, 2.0, -10_206_568.118_587, 2_000_000.000_000),
        ];
        for (lon, lat, e, n) in rows {
            let (x, y) = to_ups_in_zone(c(lon, lat), true).unwrap();
            assert!((x - e).abs() < 2e-6 && (y - n).abs() < 2e-6, "{lon} {lat}: {x} {y}, want {e} {n}");
            // Back from our own easting and northing (NGA's are rounded to the micrometer, which
            // near the pole is more than 1e-11° of longitude).
            let back = from_ups(true, x, y).unwrap();
            assert!((back.y - lat).abs() < 1e-11, "{lon} {lat}: back {back:?}");
            if lat < 90.0 {
                assert!((wrap(back.x - lon)).abs() < 1e-11, "{lon} {lat}: back {back:?}");
            }
        }
    }

    /// NGA.SIG.0012 §10.3 (south UPS, WGS 84): easting, northing → longitude, latitude, printed
    /// to 1e-10°. The 180° meridian reads as −180° here (longitudes are in [−180°, 180°)).
    #[test]
    fn inverse_matches_nga_table() {
        let rows = [
            (0.0, 0.0, -135.0, -64.916_412_333_2),
            (1_000_000.0, 0.0, -153.434_948_822_9, -70.055_294_401_4),
            (2_000_000.0, 0.0, -180.0, -72.126_361_016_3),
            (1_000_000.0, 1_000_000.0, -135.0, -77.312_079_190_8),
            (2_000_000.0, 1_000_000.0, 180.0, -81.010_663_264_5),
            (2_000_000.0, 2_000_000.0, 0.0, -90.0),
            (3_000_000.0, 2_000_000.0, 90.0, -81.010_663_264_5),
            (2_000_000.0, 3_000_000.0, 0.0, -81.010_663_264_5),
            (4_000_000.0, 4_000_000.0, 45.0, -64.916_412_333_2),
        ];
        for (e, n, lon, lat) in rows {
            let p = from_ups(false, e, n).unwrap();
            assert!((p.y - lat).abs() < 1e-9, "{e} {n}: {p:?}");
            assert!(wrap(p.x - lon).abs() < 1e-9, "{e} {n}: {p:?}");
            let (x, y) = to_ups_in_zone(p, false).unwrap();
            assert!((x - e).abs() < 1e-6 && (y - n).abs() < 1e-6, "{e} {n}: {x} {y}");
        }
    }

    /// GeographicLib's `GeoConvert -u -l -p 0` for the same places.
    #[test]
    fn readout_matches_geoconvert() {
        let cases = [
            (45.0, 85.0, "north 2392768 1607232"),
            (170.0, -85.0, "south 2096454 1452981"),
            (-180.0, 84.0, "north 2000000 2666728"),
            (0.0, 90.0, "north 2000000 2000000"),
            (0.0, -90.0, "south 2000000 2000000"),
        ];
        for (lon, lat, want) in cases {
            assert_eq!(format_ups(c(lon, lat)).as_deref(), Some(want), "{lon} {lat}");
        }
    }

    #[test]
    fn zones_follow_nga_ownership() {
        assert_eq!(ups_zone(84.0), Some(true));
        assert_eq!(ups_zone(83.999_999), None);
        assert_eq!(ups_zone(90.0), Some(true));
        assert_eq!(ups_zone(-80.0), None);
        assert_eq!(ups_zone(-80.000_001), Some(false));
        assert_eq!(ups_zone(-90.0), Some(false));
        assert_eq!(ups_zone(90.5), None);
        assert_eq!(ups_zone(f64::NAN), None);
        assert!(to_ups(c(0.0, 0.0)).is_none());
        assert!(to_ups(c(f64::NAN, 85.0)).is_none());
        assert!(to_ups(c(0.0, f64::INFINITY)).is_none());
    }

    /// The 0° and 180° meridians are on the 2,000,000 m easting, so Z and B own them; −180° is
    /// the same meridian as 180°.
    #[test]
    fn meridians_zero_and_180_are_on_the_central_easting() {
        for lon in [0.0, 180.0, -180.0, 540.0] {
            for (lat, north) in [(86.0, true), (-86.0, false)] {
                let (x, _) = to_ups_in_zone(c(lon, lat), north).unwrap();
                assert_eq!(x, FALSE_ORIGIN, "{lon} {lat}");
            }
        }
        assert!(easting_in_half('Z', FALSE_ORIGIN) && !easting_in_half('Z', FALSE_ORIGIN - 0.1));
        assert!(easting_in_half('B', FALSE_ORIGIN) && !easting_in_half('B', FALSE_ORIGIN - 0.1));
        // A typed full coordinate is rounded: Y and A take the line itself, not past it.
        assert!(easting_in_half('Y', FALSE_ORIGIN) && !easting_in_half('Y', FALSE_ORIGIN + 0.4));
        assert!(easting_in_half('A', FALSE_ORIGIN) && !easting_in_half('A', FALSE_ORIGIN + 1.0));
    }

    #[test]
    fn the_other_pole_and_hostile_values_name_no_place() {
        assert!(to_ups_in_zone(c(0.0, -90.0), true).is_none());
        assert!(to_ups_in_zone(c(0.0, 90.0), false).is_none());
        assert!(to_ups_in_zone(c(0.0, 90.5), true).is_none());
        assert!(from_ups(true, f64::NAN, 0.0).is_none());
        assert!(from_ups(true, 1e300, 1e300).is_none());
        assert!(from_ups(false, f64::INFINITY, 2e6).is_none());
        assert_eq!(from_ups(true, 2e6, 2e6), Some(c(0.0, 90.0)));
    }

    #[test]
    fn letters_follow_nga_tables() {
        // NGA.SIG.0012 §11.10: YU then YX, ZC then ZF; rows A to P.
        assert_eq!(square_letters(true, 1_300_000, 1_300_000), Some(('Y', 'R', 'A')));
        assert_eq!(square_letters(true, 1_699_999, 2_699_999), Some(('Y', 'U', 'P')));
        assert_eq!(square_letters(true, 1_700_000, 2_000_000), Some(('Y', 'X', 'H')));
        assert_eq!(square_letters(true, 2_000_000, 2_000_000), Some(('Z', 'A', 'H')));
        assert_eq!(square_letters(true, 2_300_000, 2_000_000), Some(('Z', 'F', 'H')));
        assert_eq!(square_letters(true, 2_699_999, 2_000_000), Some(('Z', 'J', 'H')));
        assert_eq!(square_letters(true, 2_700_000, 2_000_000), None);
        assert_eq!(square_letters(true, 1_299_999, 2_000_000), None);
        // §11.11: AL then AP, BC then BF; rows A to Z (no I or O).
        assert_eq!(square_letters(false, 800_000, 800_000), Some(('A', 'J', 'A')));
        assert_eq!(square_letters(false, 1_100_000, 2_000_000), Some(('A', 'P', 'N')));
        assert_eq!(square_letters(false, 2_000_000, 2_000_000), Some(('B', 'A', 'N')));
        assert_eq!(square_letters(false, 2_300_000, 3_199_999), Some(('B', 'F', 'Z')));
        assert_eq!(square_letters(false, 3_199_999, 799_999), None);
        assert_eq!(square_letters(false, 3_200_000, 2_000_000), None);
        for north in [true, false] {
            let span = if north { 13..27 } else { 8..32 };
            for col in span.clone() {
                for row in span.clone() {
                    let (h, cl, rw) = square_letters(north, col * 100_000, row * 100_000).unwrap();
                    assert_eq!(square_origin(h, cl, rw), Ok((north, col as f64 * SQUARE, row as f64 * SQUARE)));
                }
            }
        }
        assert_eq!(square_origin('Y', 'A', 'A'), Err(GridError::Square));
        assert_eq!(square_origin('Z', 'D', 'A'), Err(GridError::Square));
        assert_eq!(square_origin('B', 'A', 'I'), Err(GridError::Square));
        assert_eq!(square_origin('C', 'A', 'A'), Err(GridError::Band));
    }

    #[test]
    fn corner_squares_lie_outside_the_zone() {
        // YRA, the north lettering's south-west corner, is wholly south of 84° N.
        assert!(!cell_reaches_zone(true, 1_300_000.0, 1_300_000.0, SQUARE, 0.0));
        assert!(cell_reaches_zone(true, 2_000_000.0, 2_000_000.0, SQUARE, 0.0));
        assert!(cell_reaches_zone(false, 2_000_000.0, 2_000_000.0, SQUARE, 0.0));
        assert!(!cell_reaches_zone(false, 800_000.0, 800_000.0, SQUARE, 0.0));
    }
}
