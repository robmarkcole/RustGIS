//! The wizard's texts step.
use super::*;

/// The texts step: the title, subtitle, note, credit line and an editorial sheet's column heading.
pub(super) fn texts_step(ui: &mut egui::Ui, d: &mut Draft, map: &Map) {
    egui::Grid::new("publication_wizard_texts").num_columns(2).spacing(egui::vec2(8.0, 8.0)).show(ui, |ui| {
        let line = |ui: &mut egui::Ui, d: &mut Draft, label: &str, field: Field| {
            let caption = ui.label(label);
            let edit = egui::TextEdit::singleline(d.field_mut(field)).char_limit(MAX_PUBLICATION_TEXT).desired_width(field_width(ui));
            let r = ui.add(edit).labelled_by(caption.id);
            d.edited(&r, field);
            ui.end_row();
        };
        let label = ui.label(tl!("Title"));
        let title = ui
            .add(
                egui::TextEdit::singleline(&mut d.title)
                    .hint_text(tl!("No title"))
                    .char_limit(MAX_PUBLICATION_TEXT)
                    .desired_width(field_width(ui)),
            )
            .labelled_by(label.id);
        d.title_typed |= title.changed();
        d.edited(&title, Field::Title);
        ui.end_row();
        line(ui, d, tl!("Subtitle"), Field::Subtitle);
        let label = ui.label(tl!("Note"));
        let note = ui
            .add(egui::TextEdit::multiline(&mut d.note).desired_rows(3).char_limit(MAX_PUBLICATION_TEXT).desired_width(field_width(ui)))
            .labelled_by(label.id);
        d.edited(&note, Field::Note);
        ui.end_row();
        let label = ui.label(tl!("Credit line"));
        let author = ui
            .add_enabled(
                !d.off(Field::Author),
                egui::TextEdit::singleline(&mut d.author)
                    .hint_text(default_credit())
                    .char_limit(MAX_PUBLICATION_TEXT)
                    .desired_width(field_width(ui)),
            )
            .labelled_by(label.id);
        d.edited(&author, Field::Author);
        ui.end_row();
        ui.label("");
        ui.checkbox(&mut d.no_author, tl!("No credit line"));
        ui.end_row();
        if d.editorial() {
            line(ui, d, tl!("Column heading"), Field::Heading(2));
        }
    });
    statistic_menu(ui, d, map, Field::Note);
    limit_note(ui, d, &[Field::Title, Field::Subtitle, Field::Note, Field::Author, Field::Heading(2)]);
}

#[cfg(test)]
mod tests {
    use super::harness::*;
    use super::*;
    use egui_kittest::kittest::Queryable;

    /// Until the title is edited, it is the active map's name, as the menu titles a sheet: a map
    /// opened while the wizard is open titles its sheet; a title typed stays.
    #[test]
    fn the_title_follows_the_active_map_until_it_is_edited() {
        let mut app = app();
        open(&mut app);
        let mut h = harness(app);
        h.state_mut().run("map.new", json!({"name": "Rivers"})).unwrap();
        h.run_steps(2);
        assert_eq!(h.state().dialogs.publication.as_ref().unwrap().title, "Rivers");
        click(&mut h, "Next");
        click(&mut h, "Next");
        h.get_by_label("Title").focus();
        h.run_steps(1);
        h.get_by_label("Title").type_text(" basins");
        h.run_steps(2);
        assert_eq!(h.state().dialogs.publication.as_ref().unwrap().title, "Rivers basins");
        h.state_mut().run("map.new", json!({"name": "Lakes"})).unwrap();
        h.run_steps(2);
        assert_eq!(h.state().dialogs.publication.as_ref().unwrap().title, "Rivers basins", "typed, so kept");
    }

    /// The text fields enforce the engine's character limit (`MAX_PUBLICATION_TEXT`) and say so
    /// when a field reaches it, saying nothing on shorter text.
    #[test]
    fn texts_enforce_the_engines_character_limit_and_say_so() {
        let mut app = app();
        open(&mut app);
        app.dialogs.publication.as_mut().unwrap().step = Step::Texts;
        let mut h = harness(app);
        let limit_text = format!("Each field takes at most {MAX_PUBLICATION_TEXT} characters.");
        assert!(h.query_by_label(&limit_text).is_none(), "clean text shows no limit notice");
        // Fill Subtitle to two characters below the limit, then type five more: stopped at the limit.
        h.state_mut().dialogs.publication.as_mut().unwrap().subtitle = "x".repeat(MAX_PUBLICATION_TEXT - 2);
        h.run_steps(2);
        h.get_by_label("Subtitle").focus();
        h.run_steps(1);
        h.get_by_label("Subtitle").type_text("abcde");
        h.run_steps(2);
        let sub = &h.state().dialogs.publication.as_ref().unwrap().subtitle;
        assert_eq!(sub.chars().count(), MAX_PUBLICATION_TEXT, "capped at MAX_PUBLICATION_TEXT");
        assert!(sub.ends_with("xxab"), "kept up to the limit");
        assert!(h.query_by_label(&limit_text).is_some(), "limit notice shown at the cap");
    }
}
