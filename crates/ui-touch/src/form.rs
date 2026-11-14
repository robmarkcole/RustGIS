//! What the capture form reads from a layer's data design beyond its value lists: the fields its
//! constraint rules require, the values its contingent value sets leave a field, and a calendar
//! for date fields that a finger can use.

use egui::{RichText, Ui, Vec2};
use rustgis_ui_egui::rustgis_engine::data::query::{Expr, parse};
use rustgis_ui_egui::rustgis_engine::data::{Date, DateTime, FieldType, Value};
use rustgis_ui_egui::rustgis_engine::map::Layer;
use rustgis_ui_egui::theme::Tokens;
use rustgis_ui_egui::tl;
use std::collections::BTreeMap;

/// A value as the form holds it: a yes/no as `true` or `false` (what its chips store), anything
/// else as its text.
pub(crate) fn stored(value: &Value) -> String {
    match value {
        Value::Bool(b) => b.to_string(),
        other => other.to_string(),
    }
}

/// Whether the form's text `got` is the value `want`: a number by its value, so `3.0` and `03`
/// are `3` (a whole number exactly, past what a float holds); yes and no as the chips hold them;
/// anything else by its text.
fn holds(want: &Value, got: &str) -> bool {
    let number = |n: f64| got.trim().parse::<f64>().is_ok_and(|g| g == n);
    match want {
        Value::Int(i) => match got.trim().parse::<i64>() {
            Ok(g) => g == *i,
            // `3.0`: a whole number written with a fraction, within what a float holds exactly.
            Err(_) => got.trim().parse::<f64>().is_ok_and(|g| g.fract() == 0.0 && g.abs() < 9.0e15 && g as i64 == *i),
        },
        Value::Float(f) => number(*f),
        Value::Bool(b) => got.trim() == if *b { "true" } else { "false" },
        Value::Text(t) => t == got,
        other => other.to_string() == got,
    }
}

/// The fields a layer's constraint rules require to hold a value, by name, in the layer's field
/// order: those a rule asks `IS NOT NULL` of, alone or as one of the clauses joined by `AND` at
/// its top. A rule that can't be read, or that asks it only under a condition (inside an `OR`),
/// requires nothing here; the engine still checks the rule itself when the feature is added.
/// A field a calculation rule sets isn't asked for either: the engine fills it before it checks.
pub(crate) fn required(layer: &Layer) -> Vec<String> {
    use rustgis_ui_egui::rustgis_engine::map::design::RuleKind;
    let mut found = vec![false; layer.dataset.fields.len()];
    for rule in layer.design.rules.iter().filter(|r| r.kind == RuleKind::Constraint) {
        if let Ok(e) = parse(&rule.expression, &layer.dataset) {
            not_null(&e, &mut found);
        }
    }
    let calculated = |name: &str| layer.design.rules.iter().any(|r| r.kind == RuleKind::Calculation && r.field.eq_ignore_ascii_case(name));
    layer.dataset.fields.iter().zip(found).filter(|(f, on)| *on && !calculated(&f.name)).map(|(f, _)| f.name.clone()).collect()
}

/// Marks in `found` the fields `e` requires to hold a value, looking through `AND`s only.
fn not_null(e: &Expr, found: &mut [bool]) {
    match e {
        Expr::And(clauses) => clauses.iter().for_each(|c| not_null(c, found)),
        Expr::IsNull { field, negate: true } => {
            if let Some(on) = found.get_mut(*field) {
                *on = true;
            }
        }
        Expr::Not(inner) => {
            if let Expr::IsNull { field, negate: false } = inner.as_ref()
                && let Some(on) = found.get_mut(*field)
            {
                *on = true;
            }
        }
        _ => {}
    }
}

/// What the layer's contingent value sets leave `field` to take, given what `values` (the form,
/// by field name) holds for the sets' other fields, as the form stores them: `None` when the sets
/// leave it anything (it is in none, or a combination that fits leaves it open). An empty list
/// means no combination fits the other values chosen, whatever this field holds.
pub(crate) fn contingent(layer: &Layer, field: &str, values: &BTreeMap<String, String>) -> Option<Vec<String>> {
    let same = |a: &str, b: &str| a.eq_ignore_ascii_case(b);
    let mut left: Option<Vec<String>> = None;
    for set in &layer.design.contingent {
        let Some(at) = set.fields.iter().position(|f| same(f, field)) else { continue };
        // What the form holds for each of the set's fields; this one's own value doesn't narrow it.
        let chosen: Vec<Option<&str>> = set
            .fields
            .iter()
            .enumerate()
            .map(|(i, f)| {
                if i == at {
                    return None;
                }
                values.iter().find(|(name, _)| same(name, f)).map(|(_, v)| v.trim()).filter(|v| !v.is_empty())
            })
            .collect();
        let fits =
            |combo: &&Vec<Value>| combo.iter().zip(&chosen).all(|(want, got)| want.is_null() || got.is_none_or(|got| holds(want, got)));
        let mut these: Vec<String> = Vec::new();
        let mut open = false;
        for combo in set.combos.iter().filter(fits) {
            match combo.get(at) {
                Some(v) if !v.is_null() => {
                    let v = stored(v);
                    if !these.contains(&v) {
                        these.push(v);
                    }
                }
                // A combination that fits and leaves this field empty lets it take anything.
                _ => open = true,
            }
        }
        if open {
            continue;
        }
        left = Some(match left {
            None => these,
            Some(before) => before.into_iter().filter(|v| these.contains(v)).collect(),
        });
    }
    left
}

/// Today's date, in UTC: the phone's own time zone isn't asked for.
pub(crate) fn today() -> Option<Date> {
    let secs = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).ok()?.as_secs();
    Date::from_days(i64::try_from(secs / 86_400).ok()?)
}

/// This moment, in UTC (`…Z`), to the second, as a date and time field takes it.
pub(crate) fn now() -> Option<String> {
    let secs = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).ok()?.as_secs();
    let micros = i64::try_from(secs).ok()?.checked_mul(1_000_000)?;
    DateTime::with_offset(micros, 0).map(|t| t.to_string())
}

/// The day a date field's text names, if it names one: its date, or a date and time's date.
pub(crate) fn day_of(text: &str) -> Option<Date> {
    let text = text.trim();
    Date::parse(text).or_else(|| DateTime::parse(text).map(DateTime::date))
}

/// A field's text with its day set to `day`: a date for a date field; for a date and time
/// field, the time it held kept (from the eleventh character on), else the day alone, which
/// reads as its midnight.
pub(crate) fn with_day(text: &str, day: Date, ty: FieldType) -> String {
    let date = day.to_string();
    let text = text.trim();
    match (ty, text.get(10..)) {
        (FieldType::DateTime, Some(time)) if DateTime::parse(text).is_some() && !time.is_empty() => format!("{date}{time}"),
        _ => date,
    }
}

/// The month a calendar shows, as year and month.
pub(crate) type Month = (i64, i64);

/// The month before or after `month`, `step` months away, within years 1 to 9999.
pub(crate) fn step(month: Month, step: i64) -> Month {
    let index = (month.0 * 12 + month.1 - 1 + step).clamp(12, 9999 * 12 + 11);
    (index / 12, index % 12 + 1)
}

/// The calendar's rows besides its weeks: the month with its arrows, and the weekdays.
pub(crate) const CALENDAR_ROWS: usize = 8;

/// A month's calendar, a finger's width to each day: tapping a day sets `text` to it (see
/// [`with_day`]). The arrows move `month`. Says whether a day was tapped.
pub(crate) fn calendar(ui: &mut Ui, t: &Tokens, month: &mut Month, text: &mut String, ty: FieldType) -> bool {
    let width = ui.available_width();
    let gap = ui.spacing().item_spacing.x;
    let cell = ((width - 6.0 * gap) / 7.0).max(1.0);
    // A finger's height; the width is what seven columns leave (about 42 points on a narrow phone).
    let height = super::TARGET;
    let mut picked = false;
    ui.horizontal(|ui| {
        if ui.add(egui::Button::new(RichText::new("‹").size(20.0)).min_size(Vec2::new(super::TARGET, height))).clicked() {
            *month = step(*month, -1);
        }
        let name = month_name(month.1);
        let title = rustgis_ui_egui::i18n::fmt("{month} {year}", &[("month", name), ("year", &month.0.to_string())]);
        ui.add_sized([width - 3.0 * super::TARGET - 3.0 * gap, height], egui::Label::new(RichText::new(title).size(16.0).color(t.text)));
        if ui.add(egui::Button::new(RichText::new("›").size(20.0)).min_size(Vec2::new(super::TARGET, height))).clicked() {
            *month = step(*month, 1);
        }
        if let Some(day) = today()
            && ui.add(egui::Button::new(RichText::new(tl!("Today")).size(13.0)).min_size(Vec2::new(super::TARGET, height))).clicked()
        {
            *text = with_day(text, day, ty);
            let (y, m, _) = day.ymd();
            *month = (y, m);
            picked = true;
        }
    });
    let weekdays = [tl!("Mo"), tl!("Tu"), tl!("We"), tl!("Th"), tl!("Fr"), tl!("Sa"), tl!("Su")];
    ui.horizontal(|ui| {
        for name in weekdays {
            ui.add_sized([cell, 20.0], egui::Label::new(RichText::new(name).size(12.0).color(t.text_dim)));
        }
    });
    let Some(first) = Date::from_ymd(month.0, month.1, 1) else { return picked };
    // 1970-01-01 was a Thursday: Monday is 0.
    let lead = (i64::from(first.days()) + 3).rem_euclid(7);
    let chosen = day_of(text);
    let now = today();
    let mut day = 1 - lead;
    for _ in 0..6 {
        ui.horizontal(|ui| {
            for _ in 0..7 {
                match Date::from_ymd(month.0, month.1, day) {
                    Some(date) => {
                        let on = chosen == Some(date);
                        let ink = if on { t.on_accent } else { t.text };
                        let label = RichText::new(day.to_string()).size(15.0).color(ink);
                        let mut button =
                            egui::Button::new(label).min_size(Vec2::new(cell, height)).fill(if on { t.accent } else { t.input });
                        if now == Some(date) && !on {
                            button = button.stroke(egui::Stroke::new(1.5, t.accent));
                        }
                        if ui.add(button).clicked() {
                            *text = with_day(text, date, ty);
                            picked = true;
                        }
                    }
                    None => {
                        ui.add_space(cell);
                    }
                }
                day += 1;
            }
        });
    }
    picked
}

/// A month's name, translated.
fn month_name(month: i64) -> &'static str {
    match month {
        1 => tl!("January"),
        2 => tl!("February"),
        3 => tl!("March"),
        4 => tl!("April"),
        5 => tl!("May"),
        6 => tl!("June"),
        7 => tl!("July"),
        8 => tl!("August"),
        9 => tl!("September"),
        10 => tl!("October"),
        11 => tl!("November"),
        _ => tl!("December"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_ui_egui::rustgis_engine::data::{Dataset, Field, GeometryType};
    use rustgis_ui_egui::rustgis_engine::map::design::{AttributeRule, ContingentSet, RuleKind};

    fn layer() -> Layer {
        let fields = vec![
            Field::new("name", FieldType::Text),
            Field::new("kind", FieldType::Text),
            Field::new("size", FieldType::Text),
            Field::new("surveyed", FieldType::Boolean),
        ];
        Layer::new(1, Dataset::new("Trees", GeometryType::Point, fields))
    }

    fn rule(expression: &str) -> AttributeRule {
        AttributeRule { name: "r".into(), kind: RuleKind::Constraint, field: String::new(), expression: expression.into() }
    }

    #[test]
    fn a_field_a_rule_asks_is_not_null_of_is_required() {
        let mut l = layer();
        assert!(required(&l).is_empty());
        l.design.rules = vec![rule("name IS NOT NULL AND (size = 'big' OR size IS NOT NULL)"), rule("NOT surveyed IS NULL")];
        assert_eq!(required(&l), ["name", "surveyed"]);
        // A field a calculation rule fills isn't asked for.
        l.design.rules.push(AttributeRule { kind: RuleKind::Calculation, field: "name".into(), ..rule("'x'") });
        assert_eq!(required(&l), ["surveyed"]);
        // Only under a condition, or in a calculation rule, or unreadable: not required.
        l.design.rules = vec![rule("name IS NOT NULL OR kind = 'oak'"), rule("no_such_field IS NOT NULL")];
        l.design.rules.push(AttributeRule { kind: RuleKind::Calculation, ..rule("kind IS NOT NULL") });
        assert!(required(&l).is_empty());
    }

    #[test]
    fn contingent_values_narrow_a_field_to_what_fits_the_others_chosen() {
        let mut l = layer();
        let text = |s: &str| Value::Text(s.into());
        l.design.contingent = vec![ContingentSet {
            name: "kind and size".into(),
            fields: vec!["kind".into(), "size".into()],
            combos: vec![vec![text("oak"), text("big")], vec![text("oak"), text("small")], vec![text("pine"), text("tall")]],
        }];
        let mut values = BTreeMap::new();
        // Nothing chosen: every value the set has for the field; a field in no set is open.
        assert_eq!(contingent(&l, "size", &values), Some(vec!["big".into(), "small".into(), "tall".into()]));
        assert_eq!(contingent(&l, "name", &values), None);
        values.insert("kind".to_string(), "oak".to_string());
        assert_eq!(contingent(&l, "size", &values), Some(vec!["big".into(), "small".into()]));
        // A field's own value doesn't narrow it.
        values.insert("size".to_string(), "big".to_string());
        assert_eq!(contingent(&l, "size", &values), Some(vec!["big".into(), "small".into()]));
        assert_eq!(contingent(&l, "kind", &values), Some(vec!["oak".into()]));
        // A number typed another way is the same number.
        let mut numbers = layer();
        numbers.design.contingent = vec![ContingentSet {
            name: "count".into(),
            fields: vec!["kind".into(), "size".into()],
            combos: vec![vec![Value::Int(1), text("restricted")], vec![Value::Float(2.5), text("open")]],
        }];
        for typed in ["01", "1.0"] {
            let chosen = BTreeMap::from([("kind".to_string(), typed.to_string())]);
            assert_eq!(contingent(&numbers, "size", &chosen), Some(vec!["restricted".into()]), "{typed}");
        }
        // Whole numbers past a float's precision stay apart.
        numbers.design.contingent[0].combos =
            vec![vec![Value::Int(9_007_199_254_740_992), text("A")], vec![Value::Int(9_007_199_254_740_993), text("B")]];
        let chosen = BTreeMap::from([("kind".to_string(), "9007199254740993".to_string())]);
        assert_eq!(contingent(&numbers, "size", &chosen), Some(vec!["B".into()]));
        numbers.design.contingent[0].combos = vec![vec![Value::Int(1), text("restricted")], vec![Value::Float(2.5), text("open")]];
        let chosen = BTreeMap::from([("kind".to_string(), "2.50".to_string())]);
        assert_eq!(contingent(&numbers, "size", &chosen), Some(vec!["open".into()]));
        // Nothing fits: no value is left.
        values.insert("kind".to_string(), "birch".to_string());
        assert_eq!(contingent(&l, "size", &values), Some(vec![]));
        // A combination that fits and leaves the field empty lets it take anything.
        l.design.contingent[0].combos.push(vec![text("birch"), Value::Null]);
        assert_eq!(contingent(&l, "size", &values), None);
        // Yes and no are held as the chips hold them.
        l.design.contingent.push(ContingentSet {
            name: "surveyed".into(),
            fields: vec!["name".into(), "surveyed".into()],
            combos: vec![vec![text("Camp"), Value::Bool(true)]],
        });
        assert_eq!(contingent(&l, "surveyed", &BTreeMap::new()), Some(vec!["true".into()]));
    }

    #[test]
    fn a_day_tapped_sets_the_date_and_keeps_a_date_times_time() {
        let day = Date::from_ymd(2026, 3, 5).unwrap();
        assert_eq!(with_day("", day, FieldType::Date), "2026-03-05");
        assert_eq!(with_day("2025-01-01T08:30:00", day, FieldType::Date), "2026-03-05");
        assert_eq!(with_day("2025-01-01T08:30:00Z", day, FieldType::DateTime), "2026-03-05T08:30:00Z");
        assert_eq!(with_day("2025-01-01", day, FieldType::DateTime), "2026-03-05");
        assert_eq!(with_day("soon", day, FieldType::DateTime), "2026-03-05");
        assert_eq!(day_of("2025-01-01T08:30:00Z"), Date::from_ymd(2025, 1, 1));
        assert_eq!(day_of("soon"), None);
        // Months step across years, and stay in years 1 to 9999.
        assert_eq!(step((2026, 1), -1), (2025, 12));
        assert_eq!(step((2026, 12), 1), (2027, 1));
        assert_eq!(step((1, 1), -1), (1, 1));
        assert_eq!(step((9999, 12), 1), (9999, 12));
        assert!(today().is_some());
        assert!(now().is_some_and(|t| t.ends_with('Z') && DateTime::parse(&t).is_some()));
    }
}
