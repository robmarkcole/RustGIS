//! Date and date-time text: the parsers, `Value::parse` for both field types, and where clauses
//! that compare a date field and a date-time field with the input as a literal. Checks what a
//! parsed value must hold: it shows as text that reads back as the same value, and comparing
//! values never panics or contradicts itself.
#![no_main]

use libfuzzer_sys::fuzz_target;
use rustgis_data::query;
use rustgis_data::{Dataset, Date, DateTime, Field, FieldType, GeometryType, Value};

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let text = text.as_ref();

    if let Some(d) = Date::parse(text) {
        assert_eq!(Date::parse(&d.to_string()), Some(d), "date {d} reads back differently");
        let _ = d.sort_key();
        let _ = d.ymd();
    }
    if let Some(t) = DateTime::parse(text) {
        let shown = t.to_string();
        let back = DateTime::parse(&shown);
        assert_eq!(back, Some(t), "date-time {text:?} shows as {shown:?}, which reads back differently");
        let _ = t.sort_key();
        let _ = t.date();
        let _ = t.is_date();
        let _ = t.offset();
    }

    let values = [
        Value::parse(text, FieldType::Date),
        Value::parse(text, FieldType::DateTime),
        Value::parse_stored(text, FieldType::Date),
        Value::parse_stored(text, FieldType::DateTime),
        Value::Text(text.to_string()),
        // Fixed values to compare against, so ordering is checked both ways, not only between
        // values read from the same text.
        Value::parse("2026-03-05", FieldType::Date),
        Value::parse("2026-03-05T09:00:00+02:00", FieldType::DateTime),
        Value::parse("2026-03-05T09:00:00", FieldType::DateTime),
        Value::Text("2026-03-05".to_string()),
    ];
    for v in &values {
        let _ = v.temporal_key();
        let shown = v.to_file_text();
        if matches!(v, Value::Date(_) | Value::DateTime(_)) {
            let ty = if matches!(v, Value::Date(_)) { FieldType::Date } else { FieldType::DateTime };
            assert_eq!(&Value::parse(&shown, ty), v, "{shown:?} reads back differently");
            let _ = v.to_json();
        }
        for w in &values {
            let (ab, ba) = (v.partial_cmp(w), w.partial_cmp(v));
            assert_eq!(ab.map(std::cmp::Ordering::reverse), ba, "{v:?} and {w:?} compare differently each way");
            let _ = v.temporal_keys(w);
        }
    }

    // Where clauses: the input as a quoted literal (quotes doubled) and as a whole clause, over a
    // date field and a date-time field.
    let mut ds = Dataset::new("t", GeometryType::None, vec![Field::new("d", FieldType::Date), Field::new("ts", FieldType::DateTime)]);
    for (d, t) in [
        ("2026-01-01", "2026-03-05T09:00:00+02:00"),
        ("0001-01-01", "9999-12-31T23:59:59.999999Z"),
        ("9999-12-31", "0001-01-01T00:00:00-18:00"),
    ] {
        ds.push(None, vec![Value::parse(d, FieldType::Date), Value::parse(t, FieldType::DateTime)]);
    }
    ds.push(None, vec![Value::Null, Value::Null]);
    let lit = text.replace('\'', "''");
    for field in ["d", "ts"] {
        for clause in [
            format!("{field} = '{lit}'"),
            format!("{field} < '{lit}'"),
            format!("{field} >= '{lit}'"),
            format!("{field} BETWEEN '{lit}' AND '{lit}'"),
            format!("{field} IN ('{lit}', '2026-01-01')"),
            format!("{field} LIKE '{lit}'"),
        ] {
            let _ = query::select(&clause, &ds);
        }
    }
    let _ = query::select(text, &ds);
});
