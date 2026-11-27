//! A map frame's section lines (#2863): where each runs, its name at both ends and its look.

use super::*;

/// A section line over a map frame (#2863): the trace of a cross-section, its name at both ends
/// (A and A').
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Section {
    /// 1 to [`MAX_SECTION_NAME`] characters, its frame's only one of that name (any case).
    pub name: String,
    pub line: SectionLine,
    pub color: Rgba,
    /// Points.
    pub width: f64,
    /// Dashed: the pattern along the line, in points ([`Dash`]); `None` is solid.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::symbology::de_dash")]
    pub dash: Option<Dash>,
}

/// Where a section line runs: across the frame, straight on the page, through a place
/// (longitude/latitude) it keeps to when the frame is moved or rescaled.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum SectionLine {
    Horizontal(Coord),
    Vertical(Coord),
}

/// The section lines a map frame holds at most (one a letter), and their longest name (characters).
pub const MAX_SECTIONS: usize = 26;
pub const MAX_SECTION_NAME: usize = 20;
/// A new section line's colour and width (points), and the least and most its width may be.
pub const SECTION_COLOR: Rgba = Rgba::rgb(0xA8, 0x43, 0x1F);
pub const SECTION_WIDTH: f64 = 1.5;
pub const SECTION_WIDTHS: (f64, f64) = (MIN_INDICATOR_WIDTH, MAX_INDICATOR_WIDTH);

impl SectionLine {
    /// The place it runs through.
    pub fn at(self) -> Coord {
        match self {
            SectionLine::Horizontal(at) | SectionLine::Vertical(at) => at,
        }
    }

    /// The same line through another place.
    pub fn through(self, at: Coord) -> SectionLine {
        match self {
            SectionLine::Horizontal(_) => SectionLine::Horizontal(at),
            SectionLine::Vertical(_) => SectionLine::Vertical(at),
        }
    }

    /// A line through `at` by its direction's name, `horizontal` or `vertical`.
    pub fn from_name(direction: &str, at: Coord) -> Option<SectionLine> {
        match direction.trim().to_ascii_lowercase().as_str() {
            "horizontal" => Some(SectionLine::Horizontal(at)),
            "vertical" => Some(SectionLine::Vertical(at)),
            _ => None,
        }
    }
}

impl Section {
    /// A section line named `name`, in the default look.
    pub fn new(name: String, line: SectionLine) -> Section {
        Section { name, line, color: SECTION_COLOR, width: SECTION_WIDTH, dash: None }
    }

    /// The first letter from A to Z that no section of `sections` is named, or `None` when all 26 are.
    pub fn free_name(sections: &[Section]) -> Option<String> {
        ('A'..='Z').map(String::from).find(|n| Section::find(sections, n).is_none())
    }

    /// The index of the section of `sections` named `name` (any case).
    pub fn find(sections: &[Section], name: &str) -> Option<usize> {
        let name = name.trim().to_lowercase();
        sections.iter().position(|s| s.name.to_lowercase() == name)
    }

    /// `sections` brought into range (a layout file is hostile input): places and widths in range,
    /// a blank or repeated name given a free letter. Whether any was cut: a name past its limit, or
    /// the sections past theirs.
    pub(super) fn clamp_all(sections: &mut Vec<Section>) -> bool {
        let mut cut = sections.len() > MAX_SECTIONS;
        sections.truncate(MAX_SECTIONS);
        for s in sections.iter_mut() {
            cut |= s.name.trim().chars().nth(MAX_SECTION_NAME).is_some();
            s.name = s.name.trim().chars().take(MAX_SECTION_NAME).collect::<String>().trim_end().to_string();
            s.width = finite_or(s.width, SECTION_WIDTH).clamp(SECTION_WIDTHS.0, SECTION_WIDTHS.1);
            let at = s.line.at();
            let x = finite_or(at.x, 0.0).clamp(-540.0, 540.0);
            // (A longitude past ±180° is the same place a turn nearer.)
            let x = if x.abs() > 180.0 { (x + 180.0).rem_euclid(360.0) - 180.0 } else { x };
            s.line = s.line.through(Coord::new(x, finite_or(at.y, 0.0).clamp(-90.0, 90.0)));
        }
        for i in 0..sections.len() {
            let free = Section::free_name(sections);
            let (before, rest) = sections.split_at_mut(i);
            if let (Some(s), Some(free)) = (rest.first_mut(), free)
                && (s.name.is_empty() || Section::find(before, &s.name).is_some())
            {
                s.name = free;
            }
        }
        cut
    }
}
