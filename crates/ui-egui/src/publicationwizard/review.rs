//! The wizard's review step.
use super::*;

/// The review step: what was picked, step by step.
pub(super) fn review_step(ui: &mut egui::Ui, d: &Draft) {
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
        if d.figure() {
            // The figure note sends a sheet's texts to the caption. Its reference doesn't go there
            // (the figure draws the default one), so it is said here.
            let unused = if d.no_reference {
                Some(tl!("\"No cartographic reference\" isn't used: a figure takes the default reference"))
            } else {
                lines(&d.reference).next().map(|_| tl!("The lines typed aren't used: a figure takes the default reference"))
            };
            if let Some(unused) = unused {
                row(tl!("Reference"), unused.to_string());
            }
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
        row(tl!("Title"), or_none(&d.title));
        row(tl!("Subtitle"), or_none(&d.subtitle));
        row(tl!("Note"), or_none(&d.note));
        let sources = lines(&d.sources).collect::<Vec<_>>().join("\n");
        row(tl!("Sources"), or_none(&sources));
        heading_row(&mut row, d, 0, tl!("Sources heading"), PUBLICATION_HEADINGS[0]);
        let typed = lines(&d.reference).collect::<Vec<_>>().join("\n");
        let reference = match d.left_out(Field::Reference) {
            Some(_) if typed.is_empty() => none.to_string(),
            Some(_) => tl!("(none; the lines typed aren't used)").to_string(),
            None if typed.is_empty() => tl!("The default: projection, datum and graticule, filled in on the sheet").to_string(),
            None => typed,
        };
        row(tl!("Reference"), reference);
        heading_row(&mut row, d, 1, tl!("Reference heading"), PUBLICATION_HEADINGS[1]);
        heading_row(&mut row, d, 2, tl!("Column heading"), none);
        let credit = if d.no_author {
            none.to_string()
        } else if d.author.trim().is_empty() {
            default_credit()
        } else {
            d.author.trim().to_string()
        };
        row(tl!("Credit line"), credit);
        let accent = d.accent.map(hex).unwrap_or_else(|| tl!("The style's own").to_string());
        row(tl!("Accent"), accent);
    });
    if d.figure() {
        figure_note(ui);
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
}
