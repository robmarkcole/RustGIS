//! The legend's limits and columns.

use super::*;

/// The most layers a legend leaves out.
pub const MAX_LEGEND_EXCLUDED: usize = 1000;

/// The most layers a legend labels.
pub const MAX_LEGEND_LABELS: usize = 1000;

/// The most characters of a legend's label for a layer.
pub const MAX_LEGEND_LABEL_CHARS: usize = 200;

/// A legend's own labels for its layers' classes (#3187): by the layer's name (any case), the
/// label the layer gives a class, and the text this legend shows instead.
///
/// A file's value of another shape is read keeping its good entries and marked `damaged`, so the
/// read never fails; [`Layout::clamp`] clears the mark and counts the element.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClassLabels {
    pub by_layer: BTreeMap<String, BTreeMap<String, String>>,
    damaged: bool,
}

impl ClassLabels {
    /// No class labelled otherwise.
    pub const NONE: ClassLabels = ClassLabels { by_layer: BTreeMap::new(), damaged: false };

    pub fn is_empty(&self) -> bool {
        self.by_layer.is_empty()
    }

    /// The labels given for the classes of the layer named `name`, in any case.
    pub fn of(&self, name: &str) -> Option<&BTreeMap<String, String>> {
        self.by_layer.get(name).or_else(|| self.by_layer.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, c)| c))
    }

    /// Brought into range, as the layers' labels are: blank ones and layers left with none go,
    /// one entry per layer in any case, at most [`MAX_LEGEND_LABELS`] classes in all, each text
    /// cut to [`MAX_LEGEND_LABEL_CHARS`]. Whether anything was cut.
    pub(super) fn clamp(&mut self) -> bool {
        // Cleared, not counted here: `Layout::clamp` counts the element as adjusted because the
        // mark differs from the copy it took before.
        self.damaged = false;
        let mut cut = false;
        let mut seen = std::collections::HashSet::new();
        let mut room = MAX_LEGEND_LABELS;
        self.by_layer.retain(|name, classes| {
            // Cut first: a text can be blank only once cut.
            classes.retain(|_, text| {
                let end = text.char_indices().nth(MAX_LEGEND_LABEL_CHARS).map_or(text.len(), |(i, _)| i);
                cut |= end < text.len();
                text.truncate(end);
                !text.trim().is_empty()
            });
            if classes.is_empty() {
                return false;
            }
            if room == 0 || !seen.insert(name.to_ascii_lowercase()) {
                cut = true;
                return false;
            }
            while classes.len() > room {
                classes.pop_last();
                cut = true;
            }
            room -= classes.len();
            true
        });
        cut
    }
}

impl Serialize for ClassLabels {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.by_layer.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ClassLabels {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<ClassLabels, D::Error> {
        use serde_json::Value;
        let mut read = ClassLabels::default();
        match Value::deserialize(d)? {
            Value::Null => {}
            Value::Object(layers) => {
                for (layer, classes) in layers {
                    let Value::Object(classes) = classes else {
                        read.damaged = true;
                        continue;
                    };
                    let mut kept = BTreeMap::new();
                    for (class, text) in classes {
                        match text {
                            Value::String(text) => {
                                kept.insert(class, text);
                            }
                            _ => read.damaged = true,
                        }
                    }
                    read.by_layer.insert(layer, kept);
                }
            }
            _ => read.damaged = true,
        }
        Ok(read)
    }
}

/// How a legend fits its rows into its box.
///
/// `Invalid` is what a file's unknown name or wrong kind of value is read as; it is never kept:
/// [`Layout::clamp`] sends it to the default and counts the element, and a save leaves it out, as
/// it does the default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LegendFit {
    /// Its columns and text size as set; what doesn't fit is said.
    #[default]
    Manual,
    /// Its text shrinks until its rows fit.
    AdjustSize,
    /// Columns are added, from those set, until its rows fit.
    AdjustColumns,
    /// Columns are added and its text shrinks until its rows fit.
    AdjustColumnsAndSize,
    /// A value read from a file that is not a name; repaired to the default on load.
    #[doc(hidden)]
    Invalid,
}

impl LegendFit {
    pub const ALL: [LegendFit; 4] = [LegendFit::Manual, LegendFit::AdjustSize, LegendFit::AdjustColumns, LegendFit::AdjustColumnsAndSize];

    pub fn code(self) -> &'static str {
        match self {
            LegendFit::Manual | LegendFit::Invalid => "manual",
            LegendFit::AdjustSize => "adjustSize",
            LegendFit::AdjustColumns => "adjustColumns",
            LegendFit::AdjustColumnsAndSize => "adjustColumnsAndSize",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            LegendFit::Manual | LegendFit::Invalid => "Manual",
            LegendFit::AdjustSize => "Shrink the text",
            LegendFit::AdjustColumns => "Add columns",
            LegendFit::AdjustColumnsAndSize => "Add columns, then shrink the text",
        }
    }

    /// `manual`, `adjustSize`, `adjustColumns` or `adjustColumnsAndSize` in any case, trimmed.
    pub fn from_name(s: &str) -> Option<LegendFit> {
        let s = s.trim();
        LegendFit::ALL.into_iter().find(|v| v.code().eq_ignore_ascii_case(s))
    }

    /// This value, or the default where it is [`LegendFit::Invalid`].
    pub fn kept(self) -> LegendFit {
        if self == LegendFit::Invalid { LegendFit::default() } else { self }
    }

    /// Whether a save leaves a legend's fit out: manual, or a value that is not a name
    /// ([`LegendFit::kept`]), which reads back as manual.
    pub fn is_manual(&self) -> bool {
        self.kept() == LegendFit::Manual
    }
}

impl Serialize for LegendFit {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.kept().code())
    }
}

impl<'de> Deserialize<'de> for LegendFit {
    /// A name as [`LegendFit::from_name`] reads it; null as the default; anything else (an
    /// unknown name, a number, a list) as `Invalid`, so the read never fails and `Layout::clamp`
    /// repairs and counts it.
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<LegendFit, D::Error> {
        let v: Option<serde_json::Value> = Deserialize::deserialize(d)?;
        Ok(match v {
            None | Some(serde_json::Value::Null) => LegendFit::default(),
            Some(serde_json::Value::String(s)) => LegendFit::from_name(&s).unwrap_or(LegendFit::Invalid),
            Some(_) => LegendFit::Invalid,
        })
    }
}

/// Whether a legend shows each layer's name over the layer's classes.
///
/// `Invalid` is what a file's value that is not `true` or `false` is read as; it is never kept:
/// [`Layout::clamp`] sends it to the default and counts the element, and a save leaves it out, as
/// it does the default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LayerNames {
    /// Each layer's name over its classes.
    #[default]
    Shown,
    /// No name over a layer's classes.
    Hidden,
    /// A value read from a file that is not `true` or `false`; repaired to the default on load.
    #[doc(hidden)]
    Invalid,
}

impl LayerNames {
    /// Whether the names are shown: unless hidden, so a value that is neither reads as shown.
    pub fn shown(self) -> bool {
        self != LayerNames::Hidden
    }

    /// This value, or the default where it is [`LayerNames::Invalid`].
    pub fn kept(self) -> LayerNames {
        if self == LayerNames::Invalid { LayerNames::default() } else { self }
    }

    /// Whether a save leaves a legend's layer names out: shown, or a value that is not `true` or
    /// `false` ([`LayerNames::kept`]), which reads back as shown.
    pub fn is_shown(&self) -> bool {
        self.kept().shown()
    }

    pub fn from_shown(shown: bool) -> LayerNames {
        if shown { LayerNames::Shown } else { LayerNames::Hidden }
    }
}

impl Serialize for LayerNames {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bool(self.kept().shown())
    }
}

impl<'de> Deserialize<'de> for LayerNames {
    /// `true` or `false`; null as the default; anything else (a name, a number, a list) as
    /// `Invalid`, so the read never fails and `Layout::clamp` repairs and counts it.
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<LayerNames, D::Error> {
        let v: Option<serde_json::Value> = Deserialize::deserialize(d)?;
        Ok(match v {
            None | Some(serde_json::Value::Null) => LayerNames::default(),
            Some(serde_json::Value::Bool(shown)) => LayerNames::from_shown(shown),
            Some(_) => LayerNames::Invalid,
        })
    }
}

/// Whether a legend ends its rows in the number of features each stands for (#3237).
///
/// `Invalid` is what a file's value that is not `true` or `false` is read as; it is never kept:
/// [`Layout::clamp`] sends it to the default and counts the element, and a save leaves it out, as
/// it does the default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FeatureCounts {
    /// No numbers.
    #[default]
    Hidden,
    /// Each class row, and a one-symbol layer's row, ends in its number.
    Shown,
    /// A value read from a file that is not `true` or `false`; repaired to the default on load.
    #[doc(hidden)]
    Invalid,
}

impl FeatureCounts {
    /// Whether the numbers are shown: a value that is neither reads as hidden.
    pub fn shown(self) -> bool {
        self == FeatureCounts::Shown
    }

    /// This value, or the default where it is [`FeatureCounts::Invalid`].
    pub fn kept(self) -> FeatureCounts {
        if self == FeatureCounts::Invalid { FeatureCounts::default() } else { self }
    }

    /// Whether a save leaves a legend's counts out: hidden, or a value that is not `true` or
    /// `false`, which reads back as hidden.
    pub fn is_hidden(&self) -> bool {
        !self.shown()
    }

    /// The setting a `"showCounts"` of `shown` gives: on, or off, which a save leaves out.
    pub fn from_shown(shown: bool) -> FeatureCounts {
        if shown { FeatureCounts::Shown } else { FeatureCounts::Hidden }
    }
}

impl Serialize for FeatureCounts {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bool(self.shown())
    }
}

impl<'de> Deserialize<'de> for FeatureCounts {
    /// `true` or `false`; null as the default; anything else (a name, a number, a list) as
    /// `Invalid`, so the read never fails and `Layout::clamp` repairs and counts it.
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<FeatureCounts, D::Error> {
        let v: Option<serde_json::Value> = Deserialize::deserialize(d)?;
        Ok(match v {
            None | Some(serde_json::Value::Null) => FeatureCounts::default(),
            Some(serde_json::Value::Bool(shown)) => FeatureCounts::from_shown(shown),
            Some(_) => FeatureCounts::Invalid,
        })
    }
}

/// The most columns a legend lays its rows in.
pub const MAX_LEGEND_COLUMNS: usize = 6;

/// The most columns a reader without `layout-legend-fit` lays a legend in.
pub const LEGEND_COLUMNS_BEFORE_FIT: usize = 3;

pub(super) fn one_column() -> usize {
    1
}

/// A legend's columns as they are drawn and kept: 1 to [`MAX_LEGEND_COLUMNS`]. The one reader for
/// the range, so a layout's repair, its capability check and the drawing agree.
pub fn legend_columns_in_range(columns: usize) -> usize {
    columns.clamp(1, MAX_LEGEND_COLUMNS)
}

pub(super) fn is_one_column(columns: &usize) -> bool {
    *columns == 1
}
