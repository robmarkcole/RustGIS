//! The area a STAC search is limited to when a bounding box isn't enough: a point, a line, a
//! polygon, or several of them merged into one search geometry (the `intersects` parameter of
//! STAC's Item Search), and the test of an item's footprint against it for static catalogs, which
//! have no search to do it.
//!
//! Merging keeps only the highest dimension present (polygons, then lines, then points) and counts
//! what it left out, so the caller can say so. Input is hostile: coordinates must be finite and on
//! the globe, and a geometry of more than [`MAX_POSITIONS`] positions is refused.

use geo::{Intersects, Orient, orient::Direction};
use rustgis_data::Geometry;
use rustgis_geom::Coord;
use serde_json::Value;

use super::Item;

/// The most positions a search geometry has: more make a request body servers refuse.
pub const MAX_POSITIONS: usize = 20_000;

/// A search geometry, and what merging left out.
#[derive(Clone, Debug, PartialEq)]
pub struct Area {
    /// Point(s), line(s) or polygon(s), in longitude/latitude, to eight decimals.
    pub geometry: Geometry,
    /// Single parts of a lower dimension than the geometry, left out of it.
    pub ignored: usize,
    /// Polygons and lines too small to be one (a ring under three positions, a line under two),
    /// left out of it.
    pub degenerate: usize,
}

/// One single-part geometry.
enum Part {
    Point(Coord),
    Line(Vec<Coord>),
    Polygon(Vec<Vec<Coord>>),
}

fn parts(g: &Geometry) -> Vec<Part> {
    match g {
        Geometry::Point(c) => vec![Part::Point(*c)],
        Geometry::MultiPoint(v) => v.iter().map(|c| Part::Point(*c)).collect(),
        Geometry::Line(v) => v.iter().map(|l| Part::Line(l.clone())).collect(),
        Geometry::Polygon(v) => v.iter().map(|p| Part::Polygon(p.clone())).collect(),
    }
}

/// Eight decimals, about a millimetre.
fn round(c: Coord) -> Coord {
    let r = |v: f64| (v * 1e8).round() / 1e8;
    Coord::new(r(c.x), r(c.y))
}

/// How many positions a geometry has.
pub fn positions(g: &Geometry) -> usize {
    match g {
        Geometry::Point(_) => 1,
        Geometry::MultiPoint(v) => v.len(),
        Geometry::Line(v) => v.iter().map(Vec::len).sum(),
        Geometry::Polygon(v) => v.iter().flatten().map(Vec::len).sum(),
    }
}

/// Whether every position is a longitude and latitude on the globe.
pub fn on_globe(g: &Geometry) -> bool {
    let ok = |c: &Coord| c.x.is_finite() && c.y.is_finite() && (-180.0..=180.0).contains(&c.x) && (-90.0..=90.0).contains(&c.y);
    match g {
        Geometry::Point(c) => ok(c),
        Geometry::MultiPoint(v) => v.iter().all(ok),
        Geometry::Line(v) => v.iter().flatten().all(ok),
        Geometry::Polygon(v) => v.iter().flatten().flatten().all(ok),
    }
}

/// The geometries merged into one search area; `None` when none has a part to search with.
///
/// Polygons are dissolved so overlapping ones give a valid geometry; points and lines become the
/// matching multi-part geometry.
pub fn merge(geometries: &[Geometry]) -> Result<Option<Area>, String> {
    let mut total = 0usize;
    for g in geometries {
        if !on_globe(g) {
            return Err("the search area has a position that isn't a longitude (-180 to 180) and latitude (-90 to 90)".into());
        }
        total = total.saturating_add(positions(g));
        if total > MAX_POSITIONS {
            return Err(format!("the search area has more than {MAX_POSITIONS} positions: simplify it or use a bounding box"));
        }
    }
    let mut points = Vec::new();
    let mut lines = Vec::new();
    let mut polygons = Vec::new();
    let mut degenerate = 0usize;
    for part in geometries.iter().flat_map(parts) {
        match part {
            Part::Point(c) => points.push(round(c)),
            Part::Line(l) if l.len() >= 2 => lines.push(l.into_iter().map(round).collect::<Vec<_>>()),
            // A ring needs three positions besides the one that closes it.
            Part::Polygon(rings) if rings.first().is_some_and(|r| r.len() >= 4) => polygons
                .push(rings.into_iter().filter(|r| r.len() >= 4).map(|r| r.into_iter().map(round).collect::<Vec<_>>()).collect::<Vec<_>>()),
            Part::Line(_) | Part::Polygon(_) => degenerate += 1,
        }
    }
    let (n_points, n_lines, n_polygons) = (points.len(), lines.len(), polygons.len());
    let (geometry, ignored) = if n_polygons > 0 {
        (dissolve(polygons), n_points + n_lines)
    } else if n_lines > 0 {
        (Geometry::Line(lines), n_points)
    } else if n_points > 0 {
        (if let [only] = points.as_slice() { Geometry::Point(*only) } else { Geometry::MultiPoint(points) }, 0)
    } else {
        return Ok(None);
    };
    if positions(&geometry) > MAX_POSITIONS {
        return Err(format!("the search area has more than {MAX_POSITIONS} positions: simplify it or use a bounding box"));
    }
    Ok(Some(Area { geometry, ignored, degenerate }))
}

fn geo_polygon(rings: &[Vec<Coord>]) -> Option<geo::Polygon<f64>> {
    let line = |r: &Vec<Coord>| geo::LineString::new(r.iter().map(|c| geo::Coord { x: c.x, y: c.y }).collect());
    let (first, holes) = rings.split_first()?;
    Some(geo::Polygon::new(line(first), holes.iter().map(line).collect()))
}

/// The polygons as one geometry, overlaps dissolved; as given when the union can't be made.
fn dissolve(polygons: Vec<Vec<Vec<Coord>>>) -> Geometry {
    if polygons.len() == 1 {
        return Geometry::Polygon(polygons);
    }
    // The overlay infers its fill rule from the first ring's winding, and files mix orientations.
    let oriented: Vec<geo::Polygon<f64>> = polygons.iter().filter_map(|p| geo_polygon(p)).map(|p| p.orient(Direction::Default)).collect();
    let union = geo::unary_union(oriented.iter());
    let ring = |l: &geo::LineString<f64>| l.coords().map(|c| round(Coord::new(c.x, c.y))).collect::<Vec<_>>();
    let merged: Vec<Vec<Vec<Coord>>> = union
        .0
        .iter()
        .map(|p| std::iter::once(p.exterior()).chain(p.interiors()).map(ring).collect())
        .filter(|r: &Vec<Vec<Coord>>| !r.is_empty())
        .collect();
    if merged.is_empty() { Geometry::Polygon(polygons) } else { Geometry::Polygon(merged) }
}

/// The geometry as GeoJSON.
pub fn to_geojson(g: &Geometry) -> Value {
    rustgis_data::io::geometry_to_geojson(g)
}

/// The geometries in GeoJSON of any of its three top-level shapes (a geometry, a feature, a
/// collection of features); at most [`MAX_FEATURES`] features are read.
pub fn geometries_of_geojson(v: &Value) -> Result<Vec<Geometry>, String> {
    geometries_of_geojson_noted(v).map(|(geometries, _)| geometries)
}

/// [`geometries_of_geojson`], and how many features or members had a null geometry, which it
/// leaves out.
pub fn geometries_of_geojson_noted(v: &Value) -> Result<(Vec<Geometry>, usize), String> {
    let mut without = 0usize;
    let mut one = |g: &Value| -> Result<Option<Geometry>, String> {
        if g.is_null() {
            without += 1;
            return Ok(None);
        }
        rustgis_data::io::geometry_from_geojson(g).map(Some).ok_or_else(|| "the search area isn't GeoJSON geometry".to_string())
    };
    let geometries = match v.get("type").and_then(Value::as_str) {
        Some("FeatureCollection") => {
            let features = v.get("features").and_then(Value::as_array).ok_or("the search area's features aren't a list")?;
            if features.len() > MAX_FEATURES {
                return Err(format!("the search area has more than {MAX_FEATURES} features: simplify it or use a bounding box"));
            }
            let mut out = Vec::new();
            for f in features {
                out.extend(one(f.get("geometry").unwrap_or(&Value::Null))?);
            }
            out
        }
        Some("Feature") => one(v.get("geometry").unwrap_or(&Value::Null))?.into_iter().collect(),
        Some("GeometryCollection") => {
            let members = v.get("geometries").and_then(Value::as_array).ok_or("the search area's geometries aren't a list")?;
            if members.len() > MAX_FEATURES {
                return Err(format!("the search area has more than {MAX_FEATURES} geometries: simplify it or use a bounding box"));
            }
            let mut out = Vec::new();
            for m in members {
                out.extend(one(m)?);
            }
            out
        }
        Some(_) => one(v)?.into_iter().collect(),
        None => return Err("the search area is GeoJSON: a geometry, a feature or a collection of features".into()),
    };
    Ok((geometries, without))
}

/// The most features or geometries one search area reads.
pub const MAX_FEATURES: usize = 100_000;

/// A search geometry ready to test items against: its extent first, then the exact test.
#[derive(Clone, Debug, PartialEq)]
pub struct Shape {
    geometry: Geometry,
    geo: geo::Geometry<f64>,
    /// West, south, east, north.
    bbox: [f64; 4],
}

fn to_geo(g: &Geometry) -> geo::Geometry<f64> {
    let c = |c: &Coord| geo::Coord { x: c.x, y: c.y };
    match g {
        Geometry::Point(p) => geo::Geometry::Point(geo::Point(c(p))),
        Geometry::MultiPoint(v) => geo::Geometry::MultiPoint(geo::MultiPoint(v.iter().map(|p| geo::Point(c(p))).collect())),
        Geometry::Line(v) => geo::Geometry::MultiLineString(geo::MultiLineString(
            v.iter().map(|l| geo::LineString::new(l.iter().map(c).collect())).collect(),
        )),
        Geometry::Polygon(v) => geo::Geometry::MultiPolygon(geo::MultiPolygon(v.iter().filter_map(|p| geo_polygon(p)).collect())),
    }
}

impl Shape {
    /// The shape of a search geometry.
    pub fn new(geometry: &Geometry) -> Shape {
        let mut bbox = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
        let mut grow = |c: &Coord| {
            bbox = [bbox[0].min(c.x), bbox[1].min(c.y), bbox[2].max(c.x), bbox[3].max(c.y)];
        };
        match geometry {
            Geometry::Point(c) => grow(c),
            Geometry::MultiPoint(v) => v.iter().for_each(grow),
            Geometry::Line(v) => v.iter().flatten().for_each(grow),
            Geometry::Polygon(v) => v.iter().flatten().flatten().for_each(grow),
        }
        Shape { geometry: geometry.clone(), geo: to_geo(geometry), bbox }
    }

    /// Its extent: west, south, east, north.
    pub fn bbox(&self) -> [f64; 4] {
        self.bbox
    }

    /// The geometry it was made from.
    pub fn geometry(&self) -> &Geometry {
        &self.geometry
    }

    /// Whether an item's footprint meets it: its extent is tested first, then its geometry. An
    /// item with no usable footprint is judged by its bbox, and one with neither is kept (as a
    /// bbox filter keeps it).
    pub fn meets(&self, item: &Item) -> bool {
        if let Some([w, s, e, n]) = item.bbox {
            let [sw, ss, se, sn] = self.bbox;
            // A box over the antimeridian (west east of east) is two: up to 180° and from -180°.
            let spans = |w: f64, e: f64| if w <= e { vec![(w, e)] } else { vec![(w, 180.0), (-180.0, e)] };
            let meets = spans(w, e).iter().any(|(a, b)| *b >= sw && *a <= se);
            if !meets || s > sn || n < ss {
                return false;
            }
        }
        let footprint = item.geometry.as_ref().and_then(rustgis_data::io::geometry_from_geojson).filter(on_globe);
        match footprint {
            Some(f) => to_geo(&f).intersects(&self.geo) || self.meets_over_the_antimeridian(item, &f),
            None => true,
        }
    }

    /// An item whose box crosses the antimeridian but whose footprint isn't split there (RFC 7946
    /// asks for it to be) spans the world the other way in a plain test. Read on a world that goes
    /// on past 180°, it is where its box says. Only for a shape on one side of 0°, where moving the
    /// western longitudes a world's width east changes nothing about the shape.
    fn meets_over_the_antimeridian(&self, item: &Item, footprint: &Geometry) -> bool {
        let Some([w, _, e, _]) = item.bbox else { return false };
        let [sw, _, se, _] = self.bbox;
        if w <= e || (sw < 0.0 && se > 0.0) {
            return false;
        }
        let footprint = shifted(footprint, |x| x < 0.0);
        let shape = if se <= 0.0 { shifted(&self.geometry, |_| true) } else { self.geometry.clone() };
        to_geo(&footprint).intersects(&to_geo(&shape))
    }
}

/// `g` with the longitudes `when` picks moved a world's width east.
fn shifted(g: &Geometry, when: impl Fn(f64) -> bool) -> Geometry {
    let c = |c: &Coord| if when(c.x) { Coord::new(c.x + 360.0, c.y) } else { *c };
    match g {
        Geometry::Point(p) => Geometry::Point(c(p)),
        Geometry::MultiPoint(v) => Geometry::MultiPoint(v.iter().map(c).collect()),
        Geometry::Line(v) => Geometry::Line(v.iter().map(|l| l.iter().map(c).collect()).collect()),
        Geometry::Polygon(v) => Geometry::Polygon(v.iter().map(|p| p.iter().map(|r| r.iter().map(c).collect()).collect()).collect()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn square(x: f64, y: f64, size: f64) -> Geometry {
        let ring =
            vec![Coord::new(x, y), Coord::new(x + size, y), Coord::new(x + size, y + size), Coord::new(x, y + size), Coord::new(x, y)];
        Geometry::Polygon(vec![vec![ring]])
    }

    fn line() -> Geometry {
        Geometry::Line(vec![vec![Coord::new(0.0, 0.0), Coord::new(1.0, 1.0)]])
    }

    #[test]
    fn the_highest_dimension_wins_and_the_rest_is_counted() {
        let all = [Geometry::Point(Coord::new(0.5, 0.5)), line(), square(0.0, 0.0, 1.0)];
        let a = merge(&all).unwrap().unwrap();
        assert!(matches!(a.geometry, Geometry::Polygon(_)));
        assert_eq!((a.ignored, a.degenerate), (2, 0));
        let a = merge(&all[..2]).unwrap().unwrap();
        assert!(matches!(a.geometry, Geometry::Line(_)));
        assert_eq!(a.ignored, 1);
        let a = merge(&all[..1]).unwrap().unwrap();
        assert_eq!((a.geometry, a.ignored), (Geometry::Point(Coord::new(0.5, 0.5)), 0), "clean input leaves nothing out");
        assert!(merge(&[]).unwrap().is_none());
    }

    #[test]
    fn overlapping_polygons_are_dissolved_and_points_become_a_multipoint() {
        let a = merge(&[square(0.0, 0.0, 2.0), square(1.0, 0.0, 2.0)]).unwrap().unwrap();
        let Geometry::Polygon(p) = &a.geometry else { panic!("not a polygon") };
        assert_eq!(p.len(), 1, "one polygon, not two overlapping ones");
        let xs: Vec<f64> = p[0][0].iter().map(|c| c.x).collect();
        assert!(xs.iter().all(|x| (0.0..=3.0).contains(x)) && xs.contains(&3.0), "{xs:?}");
        let m = merge(&[Geometry::Point(Coord::new(1.0, 1.0)), Geometry::Point(Coord::new(2.0, 2.0))]).unwrap().unwrap();
        assert!(matches!(m.geometry, Geometry::MultiPoint(ref v) if v.len() == 2));
    }

    #[test]
    fn positions_are_rounded_to_eight_decimals() {
        let a = merge(&[Geometry::Point(Coord::new(1.123456789123, -2.0))]).unwrap().unwrap();
        assert_eq!(a.geometry, Geometry::Point(Coord::new(1.12345679, -2.0)));
    }

    #[test]
    fn degenerate_parts_are_counted_not_dropped_silently() {
        let short = Geometry::Line(vec![vec![Coord::new(0.0, 0.0)]]);
        let a = merge(&[short, Geometry::Point(Coord::new(0.0, 0.0))]).unwrap().unwrap();
        assert_eq!((a.degenerate, a.ignored), (1, 0));
        assert!(merge(&[Geometry::Line(vec![vec![Coord::new(0.0, 0.0)]])]).unwrap().is_none());
    }

    #[test]
    fn hostile_areas_are_refused() {
        assert!(merge(&[Geometry::Point(Coord::new(f64::NAN, 0.0))]).is_err());
        assert!(merge(&[Geometry::Point(Coord::new(200.0, 0.0))]).is_err());
        assert!(merge(&[Geometry::Point(Coord::new(0.0, 91.0))]).is_err());
        let many = Geometry::MultiPoint((0..=MAX_POSITIONS).map(|i| Coord::new(0.0, (i % 90) as f64)).collect());
        assert!(merge(&[many]).unwrap_err().contains("more than"));
        let ok = Geometry::MultiPoint((0..MAX_POSITIONS).map(|i| Coord::new(0.0, (i % 90) as f64)).collect());
        assert!(merge(&[ok]).is_ok(), "exactly the limit is fine");
    }

    #[test]
    fn geojson_in_its_three_shapes() {
        let g = json!({"type": "Point", "coordinates": [1, 2]});
        let f = json!({"type": "Feature", "properties": {}, "geometry": g});
        let c = json!({"type": "FeatureCollection", "features": [f, {"type": "Feature", "geometry": null, "properties": {}}]});
        for v in [&g, &f, &c] {
            assert_eq!(geometries_of_geojson(v).unwrap(), vec![Geometry::Point(Coord::new(1.0, 2.0))]);
        }
        assert!(geometries_of_geojson(&json!({"type": "Point", "coordinates": "x"})).is_err());
        assert!(geometries_of_geojson(&json!([1])).is_err());
        assert!(geometries_of_geojson(&json!({"type": "FeatureCollection"})).is_err());
    }

    fn item(bbox: Option<[f64; 4]>, geometry: Option<Value>) -> Item {
        let mut v = json!({"type": "Feature", "id": "i", "properties": {"datetime": "2024-06-01T00:00:00Z"}, "assets": {}});
        if let Some(b) = bbox {
            v["bbox"] = json!(b);
        }
        if let Some(g) = geometry {
            v["geometry"] = g;
        }
        crate::stac::item_from_json(&v, None).unwrap()
    }

    #[test]
    fn an_item_is_kept_when_its_footprint_meets_the_shape_not_only_its_box() {
        // A diagonal footprint: its box covers (0.9, 0.1) but the footprint doesn't.
        let tri = json!({"type": "Polygon", "coordinates": [[[0, 0], [1, 0], [0, 1], [0, 0]]]});
        let diagonal = item(Some([0.0, 0.0, 1.0, 1.0]), Some(tri));
        assert!(!Shape::new(&Geometry::Point(Coord::new(0.9, 0.9))).meets(&diagonal), "in the box, off the footprint");
        assert!(Shape::new(&Geometry::Point(Coord::new(0.2, 0.2))).meets(&diagonal));
        assert!(!Shape::new(&Geometry::Point(Coord::new(5.0, 5.0))).meets(&diagonal), "off the box");
        assert!(Shape::new(&line()).meets(&diagonal));
        // Only a bbox: the footprint is its box. Neither: kept, as a bbox filter keeps it.
        assert!(Shape::new(&Geometry::Point(Coord::new(0.9, 0.9))).meets(&item(Some([0.0, 0.0, 1.0, 1.0]), None)));
        assert!(Shape::new(&Geometry::Point(Coord::new(50.0, 50.0))).meets(&item(None, None)));
    }

    #[test]
    fn a_footprint_not_split_at_the_antimeridian_is_where_its_box_says() {
        // The ring runs 170 → -170 the short way, as the box says; a plain test sees it span -170..170.
        let ring = json!({"type": "Polygon", "coordinates": [[[170, -20], [-170, -20], [-170, -10], [170, -10], [170, -20]]]});
        let unsplit = item(Some([170.0, -20.0, -170.0, -10.0]), Some(ring));
        for x in [175.0, -175.0] {
            assert!(Shape::new(&Geometry::Point(Coord::new(x, -15.0))).meets(&unsplit), "{x}");
        }
        assert!(!Shape::new(&Geometry::Point(Coord::new(0.0, -15.0))).meets(&unsplit), "the far side of the world");
        assert!(!Shape::new(&Geometry::Point(Coord::new(100.0, -15.0))).meets(&unsplit));
        assert!(!Shape::new(&Geometry::Point(Coord::new(175.0, 30.0))).meets(&unsplit), "off the box");
    }

    #[test]
    fn a_footprint_over_the_antimeridian_meets_a_shape_on_either_side() {
        let fiji = item(Some([170.0, -20.0, -170.0, -10.0]), None);
        assert!(Shape::new(&Geometry::Point(Coord::new(-175.0, -15.0))).meets(&fiji));
        assert!(Shape::new(&Geometry::Point(Coord::new(175.0, -15.0))).meets(&fiji));
        assert!(!Shape::new(&Geometry::Point(Coord::new(0.0, -15.0))).meets(&fiji));
    }
}
