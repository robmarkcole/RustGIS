//! The wizard's sources and reference step.
use super::*;

/// The sources step: the sources, the cartographic reference and their headings.
pub(super) fn sources_step(ui: &mut egui::Ui, d: &mut Draft, map: &Map) {
    egui::Grid::new("publication_wizard_sources").num_columns(2).spacing(egui::vec2(8.0, 8.0)).show(ui, |ui| {
        list(ui, d, tl!("Sources"), Field::Sources, "", 3);
        heading(ui, d, tl!("Sources heading"), 0);
        reference_rows(ui, d);
        heading(ui, d, tl!("Reference heading"), 1);
    });
    statistic_menu(ui, d, map, Field::Sources);
    limit_note(ui, d, &[Field::Sources, Field::Heading(0), Field::Reference, Field::Heading(1)]);
}

/// A figure's reference step: the one text it draws, with no heading.
pub(super) fn reference_step(ui: &mut egui::Ui, d: &mut Draft) {
    egui::Grid::new("publication_wizard_reference").num_columns(2).spacing(egui::vec2(8.0, 8.0)).show(ui, |ui| reference_rows(ui, d));
    limit_note(ui, d, &[Field::Reference]);
}

/// The reference's box and the tick that leaves it out, as two rows of a step's grid.
fn reference_rows(ui: &mut egui::Ui, d: &mut Draft) {
    list(ui, d, tl!("Reference"), Field::Reference, &PUBLICATION_DEFAULT_REFERENCE.join("\n"), 2);
    ui.label("");
    // Wrapped to the column: a long label would widen the narrowed window.
    let width = field_width(ui);
    ui.horizontal(|ui| {
        ui.set_max_width(width);
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
        ui.checkbox(&mut d.no_reference, tl!("No cartographic reference"));
    });
    ui.end_row();
}

/// A box taking one entry per line, with a note where it holds more than the command takes.
fn list(ui: &mut egui::Ui, d: &mut Draft, label: &str, field: Field, hint: &str, rows: usize) {
    let on = !d.off(field);
    let caption = ui.label(label);
    let edit = egui::TextEdit::multiline(d.field_mut(field))
        .hint_text(hint)
        .desired_rows(rows)
        .char_limit(MAX_PUBLICATION_TEXT)
        .desired_width(field_width(ui));
    let r = ui.add_enabled(on, edit).labelled_by(caption.id);
    d.edited(&r, field);
    ui.end_row();
    if on && lines(d.field_mut(field)).count() > MAX_PUBLICATION_LINES {
        ui.label("");
        let note = i18n::fmt("At most {limit} lines.", &[("limit", &MAX_PUBLICATION_LINES.to_string())]);
        ui.label(egui::RichText::new(note).small().weak());
        ui.end_row();
    }
}

/// A section's heading, the command's own as its hint.
fn heading(ui: &mut egui::Ui, d: &mut Draft, label: &str, at: usize) {
    let field = Field::Heading(at);
    let on = !d.off(field);
    let caption = ui.label(label);
    let edit = egui::TextEdit::singleline(d.field_mut(field))
        .hint_text(PUBLICATION_HEADINGS.get(at).copied().unwrap_or_default())
        .char_limit(MAX_PUBLICATION_TEXT)
        .desired_width(field_width(ui));
    let r = ui.add_enabled(on, edit).labelled_by(caption.id);
    d.edited(&r, field);
    ui.end_row();
}

#[cfg(test)]
mod tests {
    use super::harness::*;
    use super::*;
    use egui::accesskit::Role;
    use egui_kittest::kittest::{NodeT, Queryable};
    use rustgis_engine::map::layout::ElementKind;

    type Wizard = egui_kittest::Harness<'static, RustGisApp>;

    fn draft() -> Draft {
        Draft::new(&app().session)
    }

    /// The wizard on `step`, over the sample project (its map has vector layers).
    fn at(step: Step, set: impl FnOnce(&mut Draft)) -> Wizard {
        let mut app = app();
        open(&mut app);
        let d = app.dialogs.publication.as_mut().unwrap();
        d.step = step;
        set(d);
        harness(app)
    }

    fn wizard(h: &Wizard) -> &Draft {
        h.state().dialogs.publication.as_ref().expect("the wizard is open")
    }

    /// The first vector layer of the active map, which the statistic menu lists.
    fn layer(h: &Wizard) -> String {
        crate::textform::StatLayer::of_map(h.state().session.map()).first().expect("a vector layer").name.clone()
    }

    /// What is said of a statistic that didn't fit its field.
    fn refusal() -> String {
        format!("The statistic wasn't added: each field takes at most {MAX_PUBLICATION_TEXT} characters.")
    }

    /// Insert Statistic › the first layer › Count.
    fn insert_count(h: &mut Wizard) -> String {
        let name = layer(h);
        let token = format!("{{count:{name}}}");
        for label in ["Insert Statistic", &name, &format!("Count  {token}")] {
            h.get_by_label(label).click();
            h.run_steps(4);
        }
        token
    }

    /// Sources go one per line, trimmed, the empty lines left out; none at all sends no key.
    #[test]
    fn sources_are_sent_one_per_line() {
        let mut d = draft();
        d.sources = "A\n\n B \n".into();
        assert_eq!(d.params()["sources"], json!(["A", "B"]));
        d.sources = "  \n ".into();
        assert!(d.params().get("sources").is_none());
    }

    /// An empty reference is left to the command, one typed goes line by line, and none at all is
    /// an empty list.
    #[test]
    fn the_reference_is_the_commands_lines_or_none() {
        let mut d = draft();
        assert!(d.params().get("reference").is_none());
        d.reference = "X\nY".into();
        assert_eq!(d.params()["reference"], json!(["X", "Y"]));
        d.no_reference = true;
        assert_eq!(d.params()["reference"], json!([]));
    }

    /// Headings are sent up to the last one given, a gap filled with the command's own; the
    /// column's only for the editorial style.
    #[test]
    fn headings_are_sent_up_to_the_last_one_given() {
        let mut d = draft();
        assert!(d.params().get("headings").is_none());
        d.headings[0] = " S ".into();
        assert!(d.params().get("headings").is_none(), "no sources, so no section to head");
        d.sources = "IBGE".into();
        assert_eq!(d.params()["headings"], json!(["S"]));
        d.headings = [String::new(), "Ref".into(), String::new()];
        assert_eq!(d.params()["headings"], json!([PUBLICATION_HEADINGS[0], "Ref"]));
        d.headings = [String::new(), String::new(), "Col".into()];
        assert!(d.params().get("headings").is_none(), "no column on a classic sheet");
        d.style = "editorial".into();
        assert_eq!(d.params()["headings"], json!([PUBLICATION_HEADINGS[0], PUBLICATION_HEADINGS[1], "Col"]));
    }

    /// A heading typed for a section left out isn't sent: where a later heading follows, the
    /// command's own goes in its place.
    #[test]
    fn a_heading_typed_for_a_section_left_out_isnt_sent() {
        let mut d = draft();
        d.headings = [String::new(), "Ref".into(), String::new()];
        d.no_reference = true;
        assert!(d.params().get("headings").is_none());
        d.style = "editorial".into();
        d.headings[2] = "Col".into();
        assert_eq!(d.params()["headings"], json!([PUBLICATION_HEADINGS[0], PUBLICATION_HEADINGS[1], "Col"]));
    }

    /// A figure takes the reference, but no sources or headings.
    #[test]
    fn a_figure_takes_the_reference_but_no_sources_or_headings() {
        let mut d = draft();
        d.size = "figure-1col".into();
        assert_eq!(d.params(), json!({"profile": "figure-1col"}));
        (d.sources, d.reference) = ("A".into(), "UTM 23S\n\n X ".into());
        d.headings = ["S".into(), "R".into(), "C".into()];
        assert_eq!(d.params(), json!({"profile": "figure-1col", "reference": ["UTM 23S", "X"]}));
        d.no_reference = true;
        assert_eq!(d.params(), json!({"profile": "figure-1col", "reference": []}));
        d.size = "a4".into();
        d.no_reference = false;
        assert_eq!(d.params()["reference"], json!(["UTM 23S", "X"]), "kept for the sheet");
    }

    /// A figure's step asks for the reference alone: no sources, no headings, no statistic menu.
    #[test]
    fn a_figures_step_asks_for_the_reference_alone() {
        let mut h = at(Step::Reference, |d| d.size = "figure-1col".into());
        for label in ["Sources", "Sources heading", "Reference heading"] {
            assert!(h.query_by_role_and_label(Role::Label, label).is_none(), "{label}");
        }
        assert!(h.query_by_label("Insert Statistic").is_none());
        assert!(!h.get_by_role_and_label(Role::MultilineTextInput, "Reference").accesskit_node().is_disabled());
        h.get_by_role_and_label(Role::CheckBox, "No cartographic reference").click();
        h.run_steps(4);
        assert!(wizard(&h).no_reference);
        assert!(h.get_by_role_and_label(Role::MultilineTextInput, "Reference").accesskit_node().is_disabled());
    }

    /// The figure's step says the command's limits, and not a statistic refused on a sheet's
    /// step.
    #[test]
    fn a_figures_step_says_its_limits_only() {
        let mut h = at(Step::Reference, |d| {
            d.size = "figure-1col".into();
            d.refused = [true, true];
        });
        assert!(h.query_by_label(&refusal()).is_none());
        assert!(h.query_by_label("At most 50 lines.").is_none());
        h.state_mut().dialogs.publication.as_mut().unwrap().reference = "a\n".repeat(51);
        h.run_steps(4);
        assert!(h.query_by_label("At most 50 lines.").is_some());
        h.state_mut().dialogs.publication.as_mut().unwrap().reference = "x".repeat(MAX_PUBLICATION_TEXT);
        h.run_steps(4);
        assert!(h.query_by_label(&format!("Each field takes at most {MAX_PUBLICATION_TEXT} characters.")).is_some());
    }

    /// The cursor put in the reference on a figure's step leaves where the sheet's statistic goes.
    #[test]
    fn a_figures_step_leaves_the_sheets_statistic_target() {
        let mut h = at(Step::Reference, |d| d.size = "figure-1col".into());
        h.get_by_role_and_label(Role::MultilineTextInput, "Reference").focus();
        h.run_steps(2);
        assert_eq!(wizard(&h).targets, [None, None]);
    }

    /// The step has its fields, and No cartographic reference turns the reference's box off.
    #[test]
    fn the_step_asks_for_the_sources_the_reference_and_their_headings() {
        let mut h = at(Step::Sources, |_| {});
        // By role: the step list has a button named "Sources" too.
        for label in ["Sources", "Sources heading", "Reference", "Reference heading"] {
            assert!(h.query_by_role_and_label(Role::Label, label).is_some(), "{label}");
        }
        assert!(!h.get_by_label("Reference").accesskit_node().is_disabled());
        h.get_by_role_and_label(Role::CheckBox, "No cartographic reference").click();
        h.run_steps(4);
        assert!(wizard(&h).no_reference);
        assert!(h.get_by_label("Reference").accesskit_node().is_disabled());
    }

    /// The column's heading is asked with the texts, for the editorial style alone.
    #[test]
    fn only_an_editorial_sheet_is_asked_its_columns_heading() {
        let mut h = at(Step::Texts, |_| {});
        assert!(h.query_by_label("Column heading").is_none());
        h.state_mut().dialogs.publication.as_mut().unwrap().style = "editorial".into();
        h.run_steps(4);
        assert!(h.query_by_label("Column heading").is_some());
    }

    /// A statistic goes to the field the cursor was last in, here the subtitle.
    #[test]
    fn a_statistic_goes_to_the_field_last_focused() {
        let mut h = at(Step::Texts, |d| d.subtitle = "In ".into());
        h.get_by_label("Subtitle").focus();
        h.run_steps(2);
        let token = insert_count(&mut h);
        assert_eq!(wizard(&h).subtitle, format!("In {token}"));
        assert!(wizard(&h).note.is_empty());
    }

    /// Each step keeps its own: a field focused on the sources step leaves the texts' last one.
    #[test]
    fn each_step_keeps_the_field_last_focused_on_it() {
        let mut h = at(Step::Texts, |_| {});
        h.get_by_label("Subtitle").focus();
        h.run_steps(2);
        click(&mut h, "Next");
        h.get_by_label("Sources heading").focus();
        h.run_steps(2);
        click(&mut h, "Back");
        let token = insert_count(&mut h);
        assert_eq!(wizard(&h).subtitle, token);
        assert!(wizard(&h).note.is_empty());
        click(&mut h, "Next");
        insert_count(&mut h);
        assert_eq!(wizard(&h).headings[0], token);
    }

    /// A statistic that didn't fit stays said when a field of the other step changes.
    #[test]
    fn a_refused_statistic_stays_said_after_editing_the_other_step() {
        let full = "x".repeat(MAX_PUBLICATION_TEXT - 3);
        let mut h = at(Step::Sources, |d| (d.headings[0], d.targets[1]) = (full, Some(Field::Heading(0))));
        let limit = refusal();
        insert_count(&mut h);
        click(&mut h, "Back");
        h.get_by_label("Subtitle").focus();
        h.run_steps(2);
        h.get_by_label("Subtitle").type_text("a");
        h.run_steps(2);
        click(&mut h, "Next");
        assert!(h.query_by_label(&limit).is_some());
    }

    /// Each step keeps its own refusal: a statistic added or refused on the texts step leaves the
    /// sources step's one said.
    #[test]
    fn a_statistic_on_one_step_leaves_the_others_refusal_said() {
        let limit = refusal();
        for note_full in [false, true] {
            let full = "x".repeat(MAX_PUBLICATION_TEXT - 3);
            let mut h = at(Step::Sources, |d| {
                (d.headings[0], d.targets[1]) = (full.clone(), Some(Field::Heading(0)));
                if note_full {
                    (d.subtitle, d.targets[0]) = (full.clone(), Some(Field::Subtitle));
                }
            });
            insert_count(&mut h);
            click(&mut h, "Back");
            insert_count(&mut h);
            assert_eq!(h.query_by_label(&limit).is_some(), note_full, "the texts step says only its own");
            click(&mut h, "Next");
            assert!(h.query_by_label(&limit).is_some(), "note full: {note_full}");
        }
    }

    /// With nothing edited on the step it goes to the note, or to the sources on their step, a
    /// field edited on another step left alone.
    #[test]
    fn with_no_field_edited_a_statistic_goes_to_the_steps_own() {
        let mut h = at(Step::Texts, |_| {});
        let token = insert_count(&mut h);
        assert_eq!(wizard(&h).note, token);
        let mut h = at(Step::Sources, |d| d.targets[0] = Some(Field::Subtitle));
        let token = insert_count(&mut h);
        assert_eq!(wizard(&h).sources, token);
        assert!(wizard(&h).subtitle.is_empty());
    }

    /// A statistic put in the title is a title typed: a map opened afterwards doesn't replace it.
    #[test]
    fn a_statistic_in_the_title_keeps_the_title() {
        let mut h = at(Step::Texts, |d| d.targets[0] = Some(Field::Title));
        insert_count(&mut h);
        assert!(wizard(&h).title_typed);
    }

    /// A statistic that would take its field past the limit isn't added, and that is said.
    #[test]
    fn a_statistic_past_the_limit_isnt_added_and_its_said() {
        let full = "x".repeat(MAX_PUBLICATION_TEXT - 3);
        // A one-line field: a box this full of text would push the menu out of the step's view.
        let mut h = at(Step::Sources, |d| (d.headings[0], d.targets[1]) = (full.clone(), Some(Field::Heading(0))));
        let limit = refusal();
        assert!(h.query_by_label(&limit).is_none());
        insert_count(&mut h);
        assert_eq!(wizard(&h).headings[0], full);
        assert!(wizard(&h).sources.is_empty());
        assert!(h.query_by_label(&limit).is_some());
    }

    /// A map with no vector layers has no statistic menu.
    #[test]
    fn a_map_without_vector_layers_has_no_statistic_menu() {
        for step in [Step::Texts, Step::Sources] {
            let mut h = at(step, |_| {});
            assert!(h.query_by_label("Insert Statistic").is_some(), "{step:?}");
            h.state_mut().run("map.new", json!({"name": "Empty"})).unwrap();
            h.run_steps(4);
            assert!(h.query_by_label("Insert Statistic").is_none(), "{step:?}");
        }
    }

    /// More than the command's 50 lines is said under the box.
    #[test]
    fn more_than_fifty_lines_are_said() {
        let said = "At most 50 lines.";
        let mut h = at(Step::Sources, |d| d.sources = "a\n".repeat(50));
        assert!(h.query_by_label(said).is_none());
        h.state_mut().dialogs.publication.as_mut().unwrap().reference = "a\n".repeat(51);
        h.run_steps(4);
        assert!(h.query_by_label(said).is_some());
    }

    /// The review lists the sources, the reference and the headings, the defaults where nothing
    /// was typed and "(none)" for no reference.
    #[test]
    fn the_review_lists_sources_reference_and_headings() {
        let mut h = at(Step::Review, |d| d.sources = "IBGE\nANA".into());
        assert!(h.query_by_label("IBGE\nANA").is_some());
        let default = "The default: projection, datum and graticule, filled in on the sheet";
        assert!(h.query_by_label(default).is_some(), "said in words, not as tokens");
        assert!(h.query_by_label(PUBLICATION_HEADINGS[1]).is_some());
        let d = h.state_mut().dialogs.publication.as_mut().unwrap();
        d.no_reference = true;
        d.style = "editorial".into();
        d.headings = ["Fontes".into(), String::new(), "About".into()];
        h.run_steps(4);
        assert!(h.query_by_label(default).is_none());
        assert!(h.query_by_label("Fontes").is_some() && h.query_by_label("About").is_some());
    }

    /// A heading typed for a section left out (no sources, no reference, a column on a sheet that
    /// isn't editorial) is listed as not used.
    #[test]
    fn the_review_says_a_heading_of_a_section_left_out_isnt_used() {
        let mut h = at(Step::Review, |d| {
            d.headings = ["Fontes".into(), "Base".into(), "About".into()];
            d.no_reference = true;
        });
        for said in ["Fontes (not used: no sources)", "Base (not used: no reference)", "About (not used: not an editorial sheet)"] {
            assert!(h.query_by_label(said).is_some(), "{said}");
        }
        let d = h.state_mut().dialogs.publication.as_mut().unwrap();
        (d.sources, d.no_reference, d.style) = ("IBGE".into(), false, "editorial".into());
        h.run_steps(4);
        assert!(h.query_by_label_contains("not used").is_none());
        for sent in ["Fontes", "Base", "About"] {
            assert!(h.query_by_label(sent).is_some(), "{sent}");
        }
    }

    /// Over every way a section is left out, a heading typed is either sent or said not to be
    /// used: never neither, never both.
    #[test]
    fn every_typed_heading_is_sent_or_said_not_to_be_used() {
        for sources in ["", "IBGE"] {
            for no_reference in [false, true] {
                for style in ["classic", "editorial"] {
                    let mut h = at(Step::Review, |d| {
                        d.headings = ["H0".into(), "H1".into(), "H2".into()];
                        (d.sources, d.no_reference, d.style) = (sources.into(), no_reference, style.into());
                    });
                    h.run_steps(2);
                    let sent = wizard(&h).params()["headings"].clone();
                    for i in 0..3 {
                        let typed = format!("H{i}");
                        let is_sent = sent.as_array().is_some_and(|a| a.iter().any(|v| v == typed.as_str()));
                        let said = h.query_by_label_contains(&format!("{typed} (not used")).is_some();
                        assert_ne!(is_sent, said, "{typed}: sources {sources:?}, no reference {no_reference}, {style}");
                        assert_eq!(h.query_by_label(&typed).is_some(), is_sent, "{typed} listed as it is sent");
                    }
                }
            }
        }
    }

    /// Reference lines typed and then switched off are said not to be used.
    #[test]
    fn the_review_says_reference_lines_switched_off_arent_used() {
        let said = "(none; the lines typed aren't used)";
        let mut h = at(Step::Review, |d| d.no_reference = true);
        assert!(h.query_by_label(said).is_none(), "nothing typed, nothing lost");
        h.state_mut().dialogs.publication.as_mut().unwrap().reference = "UTM 23S".into();
        h.run_steps(4);
        assert!(h.query_by_label(said).is_some());
    }

    /// A figure's review lists its reference as a sheet's does: the default, the lines, none, and
    /// lines typed and then switched off said not to be used; no headings.
    #[test]
    fn a_figures_review_lists_its_reference() {
        let default = "The default: projection, datum and graticule, filled in on the sheet";
        let unused = "(none; the lines typed aren't used)";
        let mut h = at(Step::Review, |d| {
            d.size = "figure-1col".into();
            d.headings[1] = "Ref".into();
        });
        assert!(h.query_by_label(default).is_some() && h.query_by_label_contains("Ref (not used").is_none());
        h.state_mut().dialogs.publication.as_mut().unwrap().reference = "UTM 23S".into();
        h.run_steps(4);
        assert!(h.query_by_label("UTM 23S").is_some() && h.query_by_label(default).is_none());
        h.state_mut().dialogs.publication.as_mut().unwrap().no_reference = true;
        h.run_steps(4);
        assert!(h.query_by_label(unused).is_some() && h.query_by_label("UTM 23S").is_none());
        h.state_mut().dialogs.publication.as_mut().unwrap().reference.clear();
        h.run_steps(4);
        assert!(h.query_by_label("(none)").is_some() && h.query_by_label(unused).is_none());
    }

    /// Create puts a figure's reference under its map, or makes it with none, and Undo takes it
    /// away.
    #[test]
    fn create_puts_a_figures_reference_on_it() {
        for (no_reference, want) in [(false, "UTM 23S"), (true, "")] {
            let mut h = at(Step::Review, |d| {
                (d.size, d.reference, d.no_reference) = ("figure-1col".into(), "UTM 23S".into(), no_reference);
            });
            let before = h.state().session.project.layouts.len();
            click(&mut h, "Create");
            assert_eq!(h.state().session.project.layouts.len(), before + 1);
            let layout = h.state().session.project.layouts.last().unwrap();
            let text = layout.elements.iter().find(|e| e.name == "Reference").and_then(|e| match &e.kind {
                ElementKind::Text { text, .. } => Some(text.as_str()),
                _ => None,
            });
            assert_eq!(text, Some(want));
            h.state_mut().run("edit.undo", Value::Null).unwrap();
            assert_eq!(h.state().session.project.layouts.len(), before);
        }
    }

    /// Create puts the sources, the reference and the heading given on the sheet, and Undo takes
    /// it away.
    #[test]
    fn create_puts_them_on_the_sheet() {
        let mut h = at(Step::Review, |d| {
            (d.sources, d.reference) = ("IBGE\nANA".into(), "UTM 23S".into());
            d.headings[0] = "Fontes".into();
        });
        let before = h.state().session.project.layouts.len();
        click(&mut h, "Create");
        let layout = h.state().session.project.layouts.last().unwrap();
        let text = |name: &str| {
            layout.elements.iter().find(|e| e.name == name).and_then(|e| match &e.kind {
                ElementKind::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
        };
        assert_eq!(text("Sources").as_deref(), Some("IBGE\nANA"));
        assert_eq!(text("Reference").as_deref(), Some("UTM 23S"));
        assert!(layout.elements.iter().any(|e| matches!(&e.kind, ElementKind::Text { text, .. } if text.eq_ignore_ascii_case("Fontes"))));
        h.state_mut().run("edit.undo", Value::Null).unwrap();
        assert_eq!(h.state().session.project.layouts.len(), before);
    }
}
