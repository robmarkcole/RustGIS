//! World map projections computed here, on a sphere: Robinson, Equal Earth, Natural Earth, Winkel
//! Tripel and Eckert IV. The map draws in them (rustgis-map's `Projector`, on a sphere of the WGS
//! 84 equatorial radius unless the definition gives `+R`), and a file's coordinate system in the
//! three proj4rs lacks (Natural Earth, Winkel Tripel, Eckert IV) is read with them, on a sphere of
//! its datum's semi-major axis as in PROJ (opengeos/RustGIS#1701).
//!
//! The forward formulas follow the projections' published definitions. Eckert IV and Equal Earth
//! have inverse formulas; the others use Newton's method on the forward formula. Both reject points
//! outside the map's outline (no longitude/latitude maps to them).

use crate::Coord;

/// A world projection (PROJ names: `robin`, `eqearth`, `natearth`, `wintri`, `eck4`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Robinson,
    EqualEarth,
    NaturalEarth,
    WinkelTripel,
    EckertIV,
}

impl Kind {
    pub fn from_proj_name(name: &str) -> Option<Kind> {
        Some(match name {
            "robin" => Kind::Robinson,
            "eqearth" => Kind::EqualEarth,
            "natearth" => Kind::NaturalEarth,
            "wintri" => Kind::WinkelTripel,
            "eck4" => Kind::EckertIV,
            _ => return None,
        })
    }

    pub fn proj_name(self) -> &'static str {
        match self {
            Kind::Robinson => "robin",
            Kind::EqualEarth => "eqearth",
            Kind::NaturalEarth => "natearth",
            Kind::WinkelTripel => "wintri",
            Kind::EckertIV => "eck4",
        }
    }
}

/// Winkel's own standard parallel for his Tripel, acos(2/π) (about 50.46°, degrees): the one the
/// map's Winkel Tripel preset gives as its `+lat_1`, and the one the `proj` program takes for a
/// bare `+proj=wintri`. A definition used as a coordinate system, or WKT, without one is at 0, as
/// PROJ takes those (`cs2cs`, GDAL); GDAL and PROJ always write the parallel in WKT, so a file
/// says which.
pub fn winkel_parallel() -> f64 {
    std::f64::consts::FRAC_2_PI.acos().to_degrees()
}

/// Robinson's table: (X, Y) every 5° of latitude from 0° to 90°.
const ROBINSON: [(f64, f64); 19] = [
    (1.0000, 0.0000),
    (0.9986, 0.0620),
    (0.9954, 0.1240),
    (0.9900, 0.1860),
    (0.9822, 0.2480),
    (0.9730, 0.3100),
    (0.9600, 0.3720),
    (0.9427, 0.4340),
    (0.9216, 0.4958),
    (0.8962, 0.5571),
    (0.8679, 0.6176),
    (0.8350, 0.6769),
    (0.7986, 0.7346),
    (0.7597, 0.7903),
    (0.7186, 0.8435),
    (0.6732, 0.8936),
    (0.6213, 0.9394),
    (0.5722, 0.9761),
    (0.5322, 1.0000),
];

/// Robinson's X and Y at |latitude| in degrees, by cubic (Catmull-Rom) interpolation.
fn robinson_xy(lat_deg: f64) -> (f64, f64) {
    let t = (lat_deg.abs() / 5.0).clamp(0.0, 18.0);
    let i = (t.floor() as usize).min(17);
    let f = t - i as f64;
    let at = |k: isize| {
        // Mirror at the equator and extend linearly past the pole.
        let k = k.clamp(-1, 19);
        match k {
            -1 => (ROBINSON[1].0, -ROBINSON[1].1),
            19 => (2.0 * ROBINSON[18].0 - ROBINSON[17].0, 2.0 * ROBINSON[18].1 - ROBINSON[17].1),
            k => ROBINSON[k as usize],
        }
    };
    let (p0, p1, p2, p3) = (at(i as isize - 1), at(i as isize), at(i as isize + 1), at(i as isize + 2));
    let cr = |a: f64, b: f64, c: f64, d: f64| {
        0.5 * (2.0 * b + (-a + c) * f + (2.0 * a - 5.0 * b + 4.0 * c - d) * f * f + (-a + 3.0 * b - 3.0 * c + d) * f * f * f)
    };
    (cr(p0.0, p1.0, p2.0, p3.0), cr(p0.1, p1.1, p2.1, p3.1))
}

/// Forward projection of (λ, φ) in radians, as multiples of the radius. `cos_phi1` is the cosine
/// of Winkel Tripel's standard parallel; the others don't use it.
fn unit_forward(kind: Kind, lam: f64, phi: f64, cos_phi1: f64) -> (f64, f64) {
    match kind {
        Kind::Robinson => {
            let (x, y) = robinson_xy(phi.to_degrees());
            (0.8487 * x * lam, 1.3523 * y * phi.signum())
        }
        Kind::EqualEarth => {
            const A1: f64 = 1.340264;
            const A2: f64 = -0.081106;
            const A3: f64 = 0.000893;
            const A4: f64 = 0.003796;
            let m = 3.0_f64.sqrt() / 2.0;
            let th = (m * phi.sin()).asin();
            let (t2, t6, t8) = (th * th, th.powi(6), th.powi(8));
            let x = 2.0 * 3.0_f64.sqrt() * lam * th.cos() / (3.0 * (9.0 * A4 * t8 + 7.0 * A3 * t6 + 3.0 * A2 * t2 + A1));
            let y = th * (A4 * t8 + A3 * t6 + A2 * t2 + A1);
            (x, y)
        }
        Kind::NaturalEarth => {
            let p2 = phi * phi;
            let p4 = p2 * p2;
            let x = lam * (0.8707 - 0.131979 * p2 + p4 * (-0.013791 + p4 * p2 * (0.003971 - 0.001529 * p2)));
            let y = phi * (1.007226 + p2 * (0.015085 + p4 * (-0.044475 + 0.028874 * p2 - 0.005916 * p4)));
            (x, y)
        }
        Kind::WinkelTripel => {
            let alpha = (phi.cos() * (lam / 2.0).cos()).clamp(-1.0, 1.0).acos();
            let sinc = if alpha.abs() < 1e-12 { 1.0 } else { alpha.sin() / alpha };
            let x = 0.5 * (lam * cos_phi1 + 2.0 * phi.cos() * (lam / 2.0).sin() / sinc);
            let y = 0.5 * (phi + phi.sin() / sinc);
            (x, y)
        }
        Kind::EckertIV => {
            // θ + sinθ·cosθ + 2·sinθ = (2 + π/2)·sinφ. Its slope is nought at the poles, where
            // Newton's method on θ stops short and sinφ rounds away the last centimetres, so it is
            // solved as eckert_iv_g(u) = (2 + π/2)·(1 - cos v), with u = π/2 - |θ| and
            // v = π/2 - |φ|. g rises from 0 and bends upward, so Newton's method from √t settles
            // in a few steps.
            let half = std::f64::consts::FRAC_PI_2;
            let t = (2.0 + half) * 2.0 * ((half - phi.abs()) / 2.0).sin().powi(2);
            let mut u = t.sqrt().min(half);
            for _ in 0..30 {
                let d = 2.0 * u.sin() * (1.0 + u.sin());
                if d <= 0.0 {
                    break;
                }
                let step = (eckert_iv_g(u) - t) / d;
                u = (u - step).clamp(0.0, half);
                if step.abs() < 1e-15 {
                    break;
                }
            }
            let pi = std::f64::consts::PI;
            let x = 2.0 / (pi * (4.0 + pi)).sqrt() * lam * (1.0 + u.sin());
            let y = 2.0 * (pi / (4.0 + pi)).sqrt() * u.cos().copysign(phi);
            (x, y)
        }
    }
}

/// A world projection on a sphere, with a central meridian, false easting and northing, and
/// (Winkel Tripel only) a standard parallel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct World {
    pub kind: Kind,
    /// Central meridian, degrees.
    pub lon0: f64,
    /// The sphere's radius, metres.
    pub radius: f64,
    /// False easting and northing, metres.
    pub x0: f64,
    pub y0: f64,
    /// Winkel Tripel's standard parallel, degrees (a definition's `+lat_1`, 0 without one, as
    /// PROJ takes a coordinate system's); the other projections have none.
    pub lat1: f64,
}

impl World {
    /// A projection without false easting or northing, Winkel Tripel at Winkel's own parallel.
    pub fn new(kind: Kind, lon0: f64, radius: f64) -> World {
        World { kind, lon0, radius, x0: 0.0, y0: 0.0, lat1: winkel_parallel() }
    }

    pub fn forward(&self, c: Coord) -> Option<Coord> {
        if !c.is_finite() {
            return None;
        }
        // Keep ±180° (and points a rounding error past it) on their own side, so rings along the
        // map's edge don't jump across the whole map.
        let d = c.x - self.lon0;
        let d = if d.abs() <= 180.0 + 1e-6 { d.clamp(-180.0, 180.0) } else { (d + 540.0).rem_euclid(360.0) - 180.0 };
        let lam = d.to_radians();
        let phi = c.y.clamp(-90.0, 90.0).to_radians();
        let (x, y) = unit_forward(self.kind, lam, phi, self.lat1.to_radians().cos());
        let out = Coord::new(x * self.radius + self.x0, y * self.radius + self.y0);
        out.is_finite().then_some(out)
    }

    /// The longitude and latitude at `m`; `None` when no point maps to it. Eckert IV and Equal
    /// Earth have inverse formulas; the others use Newton's method from a starting guess.
    pub fn inverse(&self, m: Coord) -> Option<Coord> {
        if !m.is_finite() {
            return None;
        }
        let (tx, ty) = ((m.x - self.x0) / self.radius, (m.y - self.y0) / self.radius);
        let (lam, phi) = match self.kind {
            Kind::EckertIV => eckert_iv_inverse(tx, ty)?,
            Kind::EqualEarth => equal_earth_inverse(tx, ty)?,
            _ => self.newton_inverse(tx, ty)?,
        };
        // Within ±180° as PROJ gives it, as `Sinusoidal` and `Eqc` do: a whole world's east edge
        // is 180°, not -180° (a rounding error past it too, as in `forward`).
        let lon = lam.to_degrees() + self.lon0;
        let lon = if lon.abs() <= 180.0 + 1e-9 { lon.clamp(-180.0, 180.0) } else { (lon + 540.0).rem_euclid(360.0) - 180.0 };
        Some(Coord::new(lon, phi.to_degrees()))
    }

    /// Newton's method on the forward formula for (λ, φ) in radians at (tx, ty), multiples of the
    /// radius. Near a pole y barely changes with latitude, so a point found this way can be metres
    /// off there; Eckert IV and Equal Earth don't use it.
    fn newton_inverse(&self, tx: f64, ty: f64) -> Option<(f64, f64)> {
        let cos_phi1 = self.lat1.to_radians().cos();
        let forward = |lam: f64, phi: f64| unit_forward(self.kind, lam, phi, cos_phi1);
        let (pi, half) = (std::f64::consts::PI, std::f64::consts::FRAC_PI_2);
        let (mut lam, mut phi) = ((tx / 0.85).clamp(-pi, pi), (ty / 1.2).clamp(-half, half));
        let h = 1e-7;
        for _ in 0..40 {
            let (fx, fy) = forward(lam, phi);
            let (ex, ey) = (fx - tx, fy - ty);
            if ex.abs() + ey.abs() < 1e-12 {
                break;
            }
            // Numerical Jacobian, stepping inward near the edges.
            let dl = if lam + h > pi { -h } else { h };
            let dp = if phi + h > half { -h } else { h };
            let (ax, ay) = forward(lam + dl, phi);
            let (bx, by) = forward(lam, phi + dp);
            let (j11, j21) = ((ax - fx) / dl, (ay - fy) / dl);
            let (j12, j22) = ((bx - fx) / dp, (by - fy) / dp);
            let det = j11 * j22 - j12 * j21;
            if !det.is_finite() || det.abs() < 1e-15 {
                break;
            }
            lam = (lam - (j22 * ex - j12 * ey) / det).clamp(-pi, pi);
            phi = (phi - (-j21 * ex + j11 * ey) / det).clamp(-half, half);
        }
        let (fx, fy) = forward(lam, phi);
        // Off the outline Newton settles at the clamp, short of the target.
        if (fx - tx).abs() + (fy - ty).abs() > 1e-9 {
            return None;
        }
        Some((lam, phi))
    }
}

/// How far past the map's outline, in radii or radians, a point still counts as on it (rounding).
const OUTLINE_SLACK: f64 = 1e-9;

/// A longitude in radians a rounding error past ±π, clamped; `None` beyond that (off the outline).
fn on_outline(lam: f64) -> Option<f64> {
    (lam.is_finite() && lam.abs() <= std::f64::consts::PI + OUTLINE_SLACK).then(|| lam.clamp(-std::f64::consts::PI, std::f64::consts::PI))
}

/// Eckert IV's θ equation measured from the pole: with u = π/2 - |θ|,
/// (2 + π/2) - (|θ| + sin|θ|·cosθ + 2·sin|θ|) = u - sin(2u)/2 + 2·(1 - cos u), written so it
/// keeps its precision as u shrinks.
fn eckert_iv_g(u: f64) -> f64 {
    u - (2.0 * u).sin() / 2.0 + 4.0 * (u / 2.0).sin().powi(2)
}

/// Eckert IV's inverse on the unit sphere, (λ, φ) in radians, from Snyder, Map Projections: A
/// Working Manual (USGS Professional Paper 1395, 1987): sinθ = y / (2·√(π/(4+π))),
/// sinφ = (θ + sinθ·cosθ + 2·sinθ) / (2 + π/2), λ = x / ((2/√(π(4+π)))·(1 + cosθ)). φ is taken
/// from the pole, as in the forward formula: 1 - sinφ = 2·sin²(v/2) = eckert_iv_g(u) / (2 + π/2).
/// θ lies within ±π/2, so 1 + cosθ is at least 1.
fn eckert_iv_inverse(x: f64, y: f64) -> Option<(f64, f64)> {
    let (pi, half) = (std::f64::consts::PI, std::f64::consts::FRAC_PI_2);
    let (w, h) = (2.0 / (pi * (4.0 + pi)).sqrt(), 2.0 * (pi / (4.0 + pi)).sqrt());
    let s = y / h;
    if s.abs() > 1.0 + OUTLINE_SLACK {
        return None;
    }
    // cosθ from 1 - |sinθ|, which is exact, rather than 1 - sin²θ, which rounds near the poles.
    let cos_at = |a: f64| ((1.0 - a) * (1.0 + a)).max(0.0).sqrt();
    let a = s.abs().min(1.0);
    let c = cos_at(a);
    let v = 2.0 * (eckert_iv_g(c.atan2(a)) / (2.0 * (2.0 + half))).sqrt().min(1.0).asin();
    // The sides meet the pole lines at a tangent, so near a corner a rounding error in y moves
    // the side far in x: a point counts as on the outline when it is within the slack of it in
    // both x and y. From y as given, not clamped: a point past the pole line has used some of
    // its slack.
    let widest = w * pi * (1.0 + cos_at((s.abs() - OUTLINE_SLACK / h).clamp(0.0, 1.0))) + OUTLINE_SLACK;
    if x.abs() > widest {
        return None;
    }
    let lam = (x / (w * (1.0 + c))).clamp(-pi, pi);
    Some((lam, (half - v).copysign(s)))
}

/// Equal Earth's inverse on the unit sphere, (λ, φ) in radians, from Šavrič, Patterson and Jenny,
/// "The Equal Earth map projection" (International Journal of Geographical Information Science,
/// 2018): θ from y by Newton's method, then φ = asin(2·sinθ/√3) and
/// λ = 3·x·(A1 + 3·A2·θ² + 7·A3·θ⁶ + 9·A4·θ⁸) / (2·√3·cosθ). θ lies within ±π/3, so cosθ is at
/// least ½.
fn equal_earth_inverse(x: f64, y: f64) -> Option<(f64, f64)> {
    const A1: f64 = 1.340264;
    const A2: f64 = -0.081106;
    const A3: f64 = 0.000893;
    const A4: f64 = 0.003796;
    let third = std::f64::consts::FRAC_PI_3;
    let y_of = |t: f64| t * (A1 + t * t * (A2 + t.powi(4) * (A3 + A4 * t * t)));
    let dy = |t: f64| A1 + 3.0 * A2 * t * t + 7.0 * A3 * t.powi(6) + 9.0 * A4 * t.powi(8);
    let edge = y_of(third);
    if y.abs() > edge * (1.0 + OUTLINE_SLACK) {
        return None;
    }
    // y grows with θ (dy is at least 1.1 within ±π/3), so Newton's method settles quickly.
    let mut th = y.clamp(-edge, edge) / A1;
    for _ in 0..20 {
        let step = (y_of(th) - y) / dy(th);
        th = (th - step).clamp(-third, third);
        if step.abs() < 1e-15 {
            break;
        }
    }
    let phi = (2.0 * th.sin() / 3.0_f64.sqrt()).clamp(-1.0, 1.0).asin();
    let lam = on_outline(3.0 * x * dy(th) / (2.0 * 3.0_f64.sqrt() * th.cos()))?;
    Some((lam, phi))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MERCATOR_R;

    #[test]
    fn round_trips_and_outlines() {
        for kind in [Kind::Robinson, Kind::EqualEarth, Kind::NaturalEarth, Kind::WinkelTripel, Kind::EckertIV] {
            for w in
                [World::new(kind, 0.0, MERCATOR_R), World { x0: 1_000.0, y0: -2_000.0, lat1: 0.0, ..World::new(kind, 30.0, MERCATOR_R) }]
            {
                for (lon, lat) in [(0.0, 0.0), (10.0, 50.0), (-120.0, -35.0), (179.0, 80.0), (-60.0, 10.0)] {
                    let m = w.forward(Coord::new(lon, lat)).unwrap();
                    let back = w.inverse(m).unwrap_or_else(|| panic!("{w:?} {lon},{lat}"));
                    let dl = ((back.x - lon + 540.0).rem_euclid(360.0) - 180.0).abs();
                    assert!(dl < 1e-6 && (back.y - lat).abs() < 1e-6, "{w:?} {lon},{lat}: {back:?}");
                }
                // Beyond the outline: nothing maps there.
                let (e, n) = (w.forward(Coord::new(w.lon0 + 180.0, 0.0)).unwrap(), w.forward(Coord::new(w.lon0, 90.0)).unwrap());
                let corner = Coord::new(w.x0 + 0.99 * (e.x - w.x0), w.y0 + 0.99 * (n.y - w.y0));
                assert!(w.inverse(corner).is_none(), "{w:?} corner");
                assert!(w.inverse(Coord::new(f64::NAN, 0.0)).is_none());
            }
            // The edges come back on their own side, as PROJ gives them: centred on 0°, the east
            // edge is 180° and the west -180°; centred on 30°, both are -150° (#1701).
            for (lon0, east, west) in [(0.0, 180.0, -180.0), (30.0, -150.0, -150.0)] {
                let w = World::new(kind, lon0, MERCATOR_R);
                let edge = |dir: f64| w.inverse(w.forward(Coord::new(lon0 + dir * 180.0, 0.0)).unwrap()).unwrap().x;
                assert!((edge(1.0) - east).abs() < 1e-9 && (edge(-1.0) - west).abs() < 1e-9, "{w:?}: {} {}", edge(1.0), edge(-1.0));
            }
        }
        // Robinson's published scale: the equator is 0.8487·2πR long.
        let r = World::new(Kind::Robinson, 0.0, MERCATOR_R);
        let x = r.forward(Coord::new(180.0, 0.0)).unwrap().x;
        assert!((x / (0.8487 * std::f64::consts::PI * MERCATOR_R) - 1.0).abs() < 1e-9);
        // A central meridian shifts the map.
        let shifted = World::new(Kind::EqualEarth, 150.0, MERCATOR_R);
        assert!(shifted.forward(Coord::new(150.0, 0.0)).unwrap().x.abs() < 1e-6);
    }

    /// Near a pole y barely changes with latitude, so an inverse that stops once the map
    /// coordinates are close brings a pole back metres away, or not at all off the central meridian.
    #[test]
    fn places_at_and_near_the_poles_come_back_within_a_millimetre() {
        for kind in [Kind::EckertIV, Kind::EqualEarth] {
            let w = World::new(kind, 0.0, MERCATOR_R);
            for (lon, lat) in [(0.0, 90.0), (0.0, -90.0), (100.0, 90.0), (-100.0, -90.0), (45.0, 89.99), (-170.0, -89.999)] {
                let m = w.forward(Coord::new(lon, lat)).unwrap();
                let back = w.inverse(m).unwrap_or_else(|| panic!("{kind:?} {lon},{lat}"));
                // Metres along the meridian and the parallel.
                let north = (back.y - lat).to_radians() * MERCATOR_R;
                let east = (back.x - lon).to_radians() * lat.to_radians().cos() * MERCATOR_R;
                let off = north.hypot(east);
                assert!(off < 1e-3, "{kind:?} {lon},{lat}: {back:?}, {off} m off");
            }
        }
    }

    /// Eckert IV's sides meet its pole lines at a tangent, so near a corner a rounding error in y
    /// moves the side far in x: the corners and places along the pole lines and the ±180° edges
    /// come back. A place 11 cm from a pole is only two nanometres of y from it, about one
    /// rounding step, so it comes back within the centimetre or so that y can tell apart there.
    #[test]
    fn places_on_the_eckert_iv_outline_come_back() {
        for w in [
            World::new(Kind::EckertIV, 0.0, MERCATOR_R),
            World { x0: 500_000.0, y0: 1_000_000.0, ..World::new(Kind::EckertIV, -45.0, MERCATOR_R) },
        ] {
            let mut at = vec![(0.0, 89.999_999), (120.0, -89.999_999)];
            for lat in [-90.0, 90.0] {
                at.extend([-180.0, -179.9, -90.0, 0.0, 45.0, 179.99, 180.0].map(|lon| (lon, lat)));
            }
            for lon in [-180.0, 180.0] {
                at.extend([-89.999_99, -89.9, -60.0, 0.0, 30.0, 89.999, 89.999_999_9].map(|lat| (lon, lat)));
            }
            for (dlon, lat) in at {
                let lon = w.lon0 + dlon;
                let m = w.forward(Coord::new(lon, lat)).unwrap();
                let back = w.inverse(m).unwrap_or_else(|| panic!("{w:?} {lon},{lat}: {m:?}"));
                // Metres along the meridian and the parallel.
                let north = (back.y - lat).to_radians() * MERCATOR_R;
                let dl = (back.x - lon + 540.0).rem_euclid(360.0) - 180.0;
                let east = dl.to_radians() * lat.to_radians().cos() * MERCATOR_R;
                let off = north.hypot(east);
                // About 1 cm here; within a few rounding steps of y of a pole, as another
                // platform's sine and cosine may round, 5 cm.
                assert!(off < 0.05, "{w:?} {lon},{lat}: {back:?}, {off} m off");
            }
        }
        // Past a corner by less than the slack in y but far in x: off the outline.
        let (pi, w, h) = (
            std::f64::consts::PI,
            2.0 / (std::f64::consts::PI * (4.0 + std::f64::consts::PI)).sqrt(),
            2.0 * (std::f64::consts::PI / (4.0 + std::f64::consts::PI)).sqrt(),
        );
        assert!(eckert_iv_inverse(w * pi, h).is_some());
        assert_eq!(eckert_iv_inverse(w * pi + 3e-5, h + 0.9e-9), None);
    }

    /// Against pyproj 3.8.0 (PROJ 9.8.1): 10°E 50°N and 120°W 35°S, on PROJ's unit sphere.
    #[test]
    fn against_proj() {
        let unit = |kind: Kind, lat1: f64| World { lat1, ..World::new(kind, 0.0, 1.0) };
        let (natearth, eck4) = (unit(Kind::NaturalEarth, 0.0), unit(Kind::EckertIV, 0.0));
        // PROJ's bare `+proj=wintri` (`+lat_1=0`), and Winkel's parallel.
        let (wintri0, wintri) = (unit(Kind::WinkelTripel, 0.0), unit(Kind::WinkelTripel, winkel_parallel()));
        for (w, at, want) in [
            (natearth, (10.0, 50.0), (0.133_153_446_193_299_88, 0.879_006_637_134_140_1)),
            (natearth, (-120.0, -35.0), (-1.716_472_953_691_948_3, -0.617_622_121_844_032_2)),
            (eck4, (10.0, 50.0), (0.125_017_995_405_402_45, 0.951_920_977_236_804_1)),
            (eck4, (-120.0, -35.0), (-1.637_200_915_680_158_8, -0.695_904_275_601_640_1)),
            (wintri0, (10.0, 50.0), (0.151_149_075_678_650_05, 0.873_092_595_459_548_4)),
            (wintri0, (-120.0, -35.0), (-1.940_535_950_688_134_1, -0.666_578_076_704_930_9)),
            (wintri, (10.0, 50.0), (0.119_438_168_634_489_14, 0.873_092_595_459_548_4)),
            (wintri, (-120.0, -35.0), (-1.560_005_066_158_203_2, -0.666_578_076_704_930_9)),
        ] {
            let m = w.forward(Coord::new(at.0, at.1)).unwrap();
            assert!((m.x - want.0).abs() < 1e-9 && (m.y - want.1).abs() < 1e-9, "{w:?}: {m:?}");
        }
    }
}
