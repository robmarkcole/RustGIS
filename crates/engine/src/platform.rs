//! What a phone provides that a computer's window doesn't: the system's file picker and (to
//! come) location and the camera. A mobile shell implements [`Platform`] for
//! its system; a front end asks through it and collects what happened with
//! [`Platform::take_events`], as nothing here waits for the user.
//!
//! Location and the camera are added with their first implementation (#1902, phase 3).

use std::path::PathBuf;

/// What the system did since the front end last asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// A file the user picked, copied to where it can be opened by path.
    Picked {
        /// What [`Platform::pick_files`] was asked for.
        purpose: String,
        /// The file's name, as the user saw it.
        name: String,
        /// Where its copy is.
        path: PathBuf,
    },
    /// A picker closed without a choice.
    Cancelled,
    /// A file was written where the user chose.
    Saved {
        /// The name it was offered under.
        name: String,
    },
    /// Something went wrong: the text is for the user.
    Failed(String),
}

impl Event {
    /// Reads an event a shell passed as one line of tab-separated fields (`picked`, `cancelled`,
    /// `saved`, `failed`). A line that is none of these is a failure that quotes it: nothing a
    /// shell reports is dropped.
    pub fn parse(line: &str) -> Event {
        let fields: Vec<&str> = line.split('\t').collect();
        match fields.as_slice() {
            ["picked", purpose, name, path] => {
                Event::Picked { purpose: purpose.to_string(), name: name.to_string(), path: PathBuf::from(path) }
            }
            ["cancelled"] => Event::Cancelled,
            // A name is the user's own text and may hold a tab: the rest of the line is the name.
            ["saved", name @ ..] if !name.is_empty() => Event::Saved { name: name.join("\t") },
            ["failed", text @ ..] => Event::Failed(text.join("\t")),
            _ => Event::Failed(format!("The system reported something RustGIS doesn't know: {line}")),
        }
    }
}

/// The phone, as a front end sees it.
pub trait Platform {
    /// Asks the user for files with the system's picker. `purpose` says what they are for (the
    /// front end's own word, returned with each file); each file arrives as an [`Event::Picked`],
    /// a closed picker as [`Event::Cancelled`].
    fn pick_files(&self, purpose: &str) -> Result<(), String>;

    /// Asks the user where to write `bytes`, offered under `name`, and writes them there:
    /// [`Event::Saved`] or [`Event::Failed`] follows. `Ok` means the user was asked, not that the
    /// file is written.
    fn save_file(&self, name: &str, bytes: Vec<u8>) -> Result<(), String>;

    /// What happened since the last call, oldest first.
    fn take_events(&self) -> Vec<Event>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_are_read_from_their_lines() {
        assert_eq!(
            Event::parse("picked\tdata\tparcels.gpkg\t/data/picked/1/parcels.gpkg"),
            Event::Picked { purpose: "data".into(), name: "parcels.gpkg".into(), path: PathBuf::from("/data/picked/1/parcels.gpkg") }
        );
        assert_eq!(Event::parse("cancelled"), Event::Cancelled);
        assert_eq!(Event::parse("saved\tmap.png"), Event::Saved { name: "map.png".into() });
        assert_eq!(Event::parse("saved\tmy\tmap.png"), Event::Saved { name: "my\tmap.png".into() });
        // A failure's text is kept whole, tabs and all.
        assert_eq!(Event::parse("failed\ta.gpkg wasn't opened:\tno room"), Event::Failed("a.gpkg wasn't opened:\tno room".into()));
    }

    #[test]
    fn an_unknown_line_is_a_failure_that_quotes_it() {
        for line in ["", "picked\tdata", "moved\tx", "saved"] {
            match Event::parse(line) {
                Event::Failed(text) => assert!(text.ends_with(line), "{text}"),
                other => panic!("{line:?} read as {other:?}"),
            }
        }
    }
}
