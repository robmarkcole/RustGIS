//! The wizard's review step.
use super::map::{area_label, name_label};
use super::*;

/// The review step: what was picked, step by step.
pub(super) fn review_step(ui: &mut egui::Ui, s: &Session, d: &Draft) {
    let none = tl!("(none)");
    let or_none = |t: &str| if t.trim().is_empty() { none.to_string() } else { t.trim().to_string() };
    egui::Grid::new("publication_wizard_review").num_columns(2).spacing(egui::vec2(12.0, 6.0)).show(ui, |ui| {
        let mut row = |key: &str, value: String| {
            ui.label(egui::RichText::new(key).weak());
            // A long value wraps in what the keys leave rather than widening the window.
            ui.add(egui::Label::new(value).wrap());
            ui.end_row();
        };
        row(tl!("Size"), size_label(&d.size));
        row(tl!("Map extent"), area_label(s, &d.area));
        row(tl!("Name"), name_label(s, &d.name));
        if d.figure() {
            placement_rows(&mut row, s, d);
            row(tl!("Reference"), reference_value(d));
            return;
        }
        let turned = match d.orientation {
            Orientation::Auto => tl!("Auto: the way the map comes out larger"),
            Orientation::Portrait => tl!("Portrait"),
            Orientation::Landscape => tl!("Landscape"),
        };
        row(tl!("Orientation"), turned.to_string());
        let style = crate::ribbon::publication_style(&d.style).map(|s| s.0.to_string()).unwrap_or_else(|| d.style.clone());
        row(tl!("Style"), style);
        placement_rows(&mut row, s, d);
        row(tl!("Title"), or_none(&d.title));
        row(tl!("Subtitle"), or_none(&d.subtitle));
        row(tl!("Note"), or_none(&d.note));
        let sources = lines(&d.sources).collect::<Vec<_>>().join("\n");
        row(tl!("Sources"), or_none(&sources));
        heading_row(&mut row, d, 0, tl!("Sources heading"), PUBLICATION_HEADINGS[0]);
        row(tl!("Reference"), reference_value(d));
        heading_row(&mut row, d, 1, tl!("Reference heading"), PUBLICATION_HEADINGS[1]);
        heading_row(&mut row, d, 2, tl!("Column heading"), none);
        row(tl!("Credit line"), credit_value(d));
        let accent = d.accent.map(hex).unwrap_or_else(|| tl!("The style's own").to_string());
        row(tl!("Accent"), accent);
    });
    if d.figure() {
        figure_note(ui);
    }
}

/// The placement's row, and one more when Create also makes it the favourite.
fn placement_rows(row: &mut impl FnMut(&str, String), s: &Session, d: &Draft) {
    row(tl!("Placement"), d.arrangement.label(s));
    if d.favourite_to_set(s).is_some() {
        row(tl!("Favourite"), tl!("Create makes this arrangement the favourite").to_string());
    }
}

/// The reference as it will be drawn: the lines typed, or the command's default; left out, none,
/// but lines typed then are said not to be used.
fn reference_value(d: &Draft) -> String {
    let typed = lines(&d.reference).collect::<Vec<_>>().join("\n");
    match d.left_out(Field::Reference) {
        Some(_) if typed.is_empty() => tl!("(none)").to_string(),
        Some(_) => tl!("(none; the lines typed aren't used)").to_string(),
        None if typed.is_empty() => tl!("The default: projection, datum and graticule, filled in on the sheet").to_string(),
        None => typed,
    }
}

/// The credit line as it will be drawn: the one typed, or the command's default; switched off,
/// none, but one typed then is said not to be used.
fn credit_value(d: &Draft) -> String {
    let typed = d.author.trim();
    match d.off(Field::Author) {
        true if typed.is_empty() => tl!("(none)").to_string(),
        true => tl!("(none; the line typed isn't used)").to_string(),
        false if typed.is_empty() => default_credit(),
        false => typed.to_string(),
    }
}

/// A heading's row: the one typed, or `or` for the command's; for a section left out, none, but
/// one typed there is said not to be used rather than dropped unseen.
fn heading_row(row: &mut impl FnMut(&str, String), d: &Draft, at: usize, label: &str, or: &str) {
    let typed = d.headings[at].trim();
    let value = match d.left_out(Field::Heading(at)) {
        None if typed.is_empty() => or.to_string(),
        None => typed.to_string(),
        Some(_) if typed.is_empty() => return,
        Some(LeftOut::NoSources) => i18n::fmt("{heading} (not used: no sources)", &[("heading", typed)]),
        Some(LeftOut::NoReference) => i18n::fmt("{heading} (not used: no reference)", &[("heading", typed)]),
        Some(LeftOut::NotEditorial) => i18n::fmt("{heading} (not used: not an editorial sheet)", &[("heading", typed)]),
    };
    row(label, value);
}

#[cfg(test)]
mod tests {
    use super::harness::*;
    use super::*;
    use egui_kittest::kittest::Queryable;

    /// The review says what will be drawn: the default credit line, dated when the layout is made, and for
    /// a figure, that its texts and accent are left to the caption.
    #[test]
    fn the_review_says_what_will_be_drawn() {
        let mut app = app();
        open(&mut app);
        app.dialogs.publication.as_mut().unwrap().step = Step::Review;
        let mut h = harness(app);
        assert!(h.query_by_label("Made with RustGIS · today's date").is_some());
        assert!(h.query_by_label("Auto: the way the map comes out larger").is_some(), "the turn, decided at Create");
        h.state_mut().dialogs.publication.as_mut().unwrap().size = "figure-1col".into();
        h.run_steps(2);
        assert!(h.query_by_label_contains("its caption").is_some(), "a figure's texts aren't drawn");
    }

    /// A credit line typed and then switched off is said not to be used, as reference lines are;
    /// with none typed, the row says "(none)" and nothing more.
    #[test]
    fn the_review_says_a_credit_line_switched_off_isnt_used() {
        let default = "Made with RustGIS · today's date";
        let unused = "(none; the line typed isn't used)";
        let mut app = app();
        open(&mut app);
        app.dialogs.publication.as_mut().unwrap().step = Step::Review;
        let mut h = harness(app);
        // Other empty rows say "(none)" too: the credit row's is the one more.
        let others = h.query_all_by_label("(none)").count();
        for (author, no_author, listed) in [
            ("", false, default),
            ("J. G.", false, "J. G."),
            ("   ", false, default),
            ("J. G.", true, unused),
            ("", true, "(none)"),
            ("   ", true, "(none)"),
        ] {
            let d = h.state_mut().dialogs.publication.as_mut().unwrap();
            (d.author, d.no_author) = (author.into(), no_author);
            h.run_steps(4);
            for text in [default, "J. G.", unused] {
                assert_eq!(h.query_by_label(text).is_some(), text == listed, "{text}: {author:?}, off {no_author}");
            }
            let none = h.query_all_by_label("(none)").count();
            assert_eq!(none, others + usize::from(listed == "(none)"), "(none): {author:?}, off {no_author}");
        }
    }
}
