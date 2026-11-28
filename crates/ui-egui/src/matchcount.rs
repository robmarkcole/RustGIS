//! Select By Attributes' count of the rows a clause matches (#2946). Counting is a pass over every
//! row of the layer, so it is not done in each frame: once per clause and set of rows, on a thread
//! of its own on the desktop (the browser has none, so it counts inline, still once per change),
//! with the dialog saying "Counting matching rows…" until it lands.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use rustgis_engine::data::raster::Crs;
use rustgis_engine::data::{Dataset, Feature, query};

use crate::Slot;

/// How many rows are counted between two looks at the stop flag.
const CHUNK: usize = 4096;

/// What the dialog shows under the clause.
#[derive(Debug, PartialEq)]
pub(crate) enum Shown {
    /// The count is on its way.
    Counting,
    /// How many rows match.
    Rows(usize),
    /// Why the clause doesn't read.
    Invalid(String),
    /// Why the count could not be made, though the clause reads.
    Failed(String),
}

/// What a count came to.
#[derive(Debug, PartialEq)]
enum Answer {
    Rows(usize),
    Invalid(String),
    Failed(String),
}

impl From<Answer> for Shown {
    fn from(a: Answer) -> Shown {
        match a {
            Answer::Rows(n) => Shown::Rows(n),
            Answer::Invalid(e) => Shown::Invalid(e),
            Answer::Failed(e) => Shown::Failed(e),
        }
    }
}

/// What a count was made for: it holds only while the same layer has the same rows and fields. The
/// rows are held by their allocation, which any edit, reload, Undo or next view of a streamed
/// layer ends ([`rustgis_engine::data::Features::allocation_key`]).
struct Key {
    layer: String,
    text: String,
    rows: Weak<Vec<Feature>>,
    fields: u64,
    windowed: bool,
    held_in: Crs,
}

impl Key {
    fn is(&self, text: &str, layer: &str, ds: &Dataset) -> bool {
        self.text == text
            && self.layer == layer
            && self.windowed == ds.windowed
            && self.held_in == ds.held_in
            && self.rows.ptr_eq(&ds.features.allocation_key())
            && self.fields == crate::exprcomplete::fields_key(ds)
    }
}

enum State {
    /// On a thread of its own: `Err` in the slot is a thread the system refused.
    Counting(Slot<Result<Answer, String>>),
    Done(Answer),
}

/// The count between frames, held by the dialog (`Dialogs::matching`). Dropping it (the dialog
/// closed or another opened) stops a count still running. A layer removed while it counts keeps
/// its rows until the count ends or looks at its stop flag, and an edit to the layer in that time
/// copies the rows once (the count shares them).
#[derive(Default)]
pub struct Count {
    key: Option<Key>,
    state: Option<State>,
    stop: Arc<AtomicBool>,
    /// How many counts were started (a clause that doesn't read starts none): once per change, not
    /// once per frame.
    #[cfg(test)]
    pub(crate) starts: usize,
    /// A test's stand-in for a system with no thread to give.
    #[cfg(test)]
    pub(crate) refuse: bool,
}

impl Drop for Count {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Count {
    /// What to show for `text` over layer `layer`'s rows `ds`, each frame it is asked for. A new
    /// clause, layer or set of rows starts a count (and stops the one before); the answer is kept
    /// while they stay as they were. `off_frame`: count on a thread of its own, else here.
    pub(crate) fn of(&mut self, ctx: &egui::Context, text: &str, layer: &str, ds: &Dataset, off_frame: bool) -> Shown {
        let current = self.key.as_ref().is_some_and(|k| k.is(text, layer, ds));
        if !current {
            self.start(ctx, text, layer, ds, off_frame);
        } else if let Some(State::Counting(slot)) = &self.state {
            let landed = slot.lock().unwrap_or_else(|e| e.into_inner()).take();
            match landed {
                Some(Ok(a)) => self.state = Some(State::Done(a)),
                Some(Err(e)) => self.state = Some(State::Done(Answer::Failed(e))),
                None => {}
            }
        }
        match &self.state {
            Some(State::Done(Answer::Rows(n))) => Shown::Rows(*n),
            Some(State::Done(Answer::Invalid(e))) => Shown::Invalid(e.clone()),
            Some(State::Done(Answer::Failed(e))) => Shown::Failed(e.clone()),
            _ => {
                // The thread asks for a frame when it lands; this is for one that never does.
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
                Shown::Counting
            }
        }
    }

    fn start(&mut self, ctx: &egui::Context, text: &str, layer: &str, ds: &Dataset, off_frame: bool) {
        // Whatever was counting is of a clause or rows no longer asked about.
        self.stop.store(true, Ordering::Relaxed);
        self.stop = Arc::default();
        self.key = Some(Key {
            layer: layer.to_string(),
            text: text.to_string(),
            rows: ds.features.allocation_key(),
            fields: crate::exprcomplete::fields_key(ds),
            windowed: ds.windowed,
            held_in: ds.held_in,
        });
        // The words and fields only: no geometry is read (`parse` reads them all for `$area`), so
        // a clause that doesn't read is said at once and costs nothing. The full `parse` is the
        // count's first step, so what it refuses is said as it always was.
        if let Err(e) = query::parse_fields(text, ds) {
            self.state = Some(State::Done(Answer::Invalid(e)));
            return;
        }
        #[cfg(test)]
        {
            self.starts += 1;
        }
        if !off_frame {
            self.state = Some(State::Done(now(text, ds)));
            return;
        }
        let slot: Slot<Result<Answer, String>> = Slot::default();
        let land = slot.clone();
        let (owned, rows, stop, repaint) = (text.to_string(), ds.clone(), self.stop.clone(), ctx.clone());
        let work = move || {
            if let Some(a) = guarded(|| count(&owned, &rows, &stop)) {
                *land.lock().unwrap_or_else(|e| e.into_inner()) = Some(Ok(a));
                repaint.request_repaint();
            }
        };
        #[cfg(test)]
        if self.refuse {
            crate::work_off_frame_with(|_| Err(std::io::Error::other("no threads left")), &slot, work);
        } else {
            crate::work_off_frame("rustgis-select-count", &slot, work);
        }
        #[cfg(not(test))]
        crate::work_off_frame("rustgis-select-count", &slot, work);
        // A thread the system refused is said as it is: counting here would hold the frame for as
        // long as the layer is large, with no Cancel to reach.
        let refused = match &*slot.lock().unwrap_or_else(|e| e.into_inner()) {
            Some(Err(e)) => Some(e.clone()),
            _ => None,
        };
        self.state = Some(match refused {
            Some(e) => State::Done(Answer::Failed(e)),
            None => State::Counting(slot),
        });
    }

    /// The flag that stops the count on its way (a test watches it).
    #[cfg(test)]
    pub(crate) fn stop_flag(&self) -> Arc<AtomicBool> {
        self.stop.clone()
    }

    /// Whether a count is on its way.
    #[cfg(test)]
    pub(crate) fn counting(&self) -> bool {
        matches!(self.state, Some(State::Counting(_)))
    }
}

/// Runs `f`, giving a bug in the count as an answer, not a thread that ends with nothing landed
/// (no command's catch reaches it).
fn guarded(f: impl FnOnce() -> Option<Answer>) -> Option<Answer> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
        .unwrap_or_else(|_| Some(Answer::Failed(tl!("Counting the matching rows failed unexpectedly").to_string())))
}

/// The count at once, here (the browser's, on its one thread).
fn now(text: &str, ds: &Dataset) -> Answer {
    let never = AtomicBool::new(false);
    guarded(|| count(text, ds, &never)).unwrap_or_else(|| Answer::Failed(tl!("Counting the matching rows failed unexpectedly").to_string()))
}

/// How many rows `text` matches, as `query::select(text, ds)` has them: looks at `stop` before it
/// starts and between chunks of [`CHUNK`] rows, and gives nothing once it is set. One row's
/// evaluation, and the full `parse`'s first read of the geometries (`$area`), are not interrupted.
fn count(text: &str, ds: &Dataset, stop: &AtomicBool) -> Option<Answer> {
    count_between(text, ds, stop, || {})
}

/// [`count`], calling `between` after each chunk (a test sets the stop there).
fn count_between(text: &str, ds: &Dataset, stop: &AtomicBool, mut between: impl FnMut()) -> Option<Answer> {
    // A count already superseded (a keystroke ago) doesn't start `parse`'s read of the geometries.
    if stop.load(Ordering::Relaxed) {
        return None;
    }
    let e = match query::parse(text, ds) {
        Ok(e) => e,
        Err(e) => return Some(Answer::Invalid(e)),
    };
    let rows = ds.features.len();
    let mut n = 0;
    let mut from = 0;
    while from < rows {
        if stop.load(Ordering::Relaxed) {
            return None;
        }
        let to = from.saturating_add(CHUNK).min(rows);
        n += (from..to).filter(|r| query::eval(&e, ds, *r)).count();
        from = to;
        between();
    }
    Some(Answer::Rows(n))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_engine::data::{Field, FieldType, GeometryType, Value};

    /// `rows` points with an integer field `n` counting up from 0.
    fn layer(rows: usize) -> Dataset {
        let mut ds = Dataset::new("t", GeometryType::Point, vec![Field::new("n", FieldType::Integer)]);
        for i in 0..rows {
            ds.push(None, vec![Value::Int(i as i64)]);
        }
        ds
    }

    /// The count is the number of rows `select` gives, and a clause that doesn't read gives the
    /// error `select` gives.
    #[test]
    fn the_count_is_what_select_gives() {
        let ds = layer(10_000);
        let never = AtomicBool::new(false);
        for (clause, expected) in [("n >= 9000", 1000), ("n < 0", 0), ("n >= 0", 10_000), ("n IN (1, 2, 3)", 3), ("n IS NULL", 0)] {
            assert_eq!(count(clause, &ds, &never), Some(Answer::Rows(expected)), "{clause}");
            assert_eq!(now(clause, &ds), Answer::Rows(expected), "{clause} at once");
            assert_eq!(query::select(clause, &ds).map(|r| r.len()), Ok(expected), "{clause} by select");
        }
        for bad in ["nope = 1", "", "n >"] {
            let said = query::select(bad, &ds).map(|r| r.len()).unwrap_err();
            assert_eq!(count(bad, &ds, &never), Some(Answer::Invalid(said.clone())), "{bad:?}");
            assert_eq!(now(bad, &ds), Answer::Invalid(said), "{bad:?} at once");
        }
        assert_eq!(now("nope = 1", &ds), Answer::Invalid("no field named 'nope'".to_string()));
    }

    /// A stop that is set leaves no answer, even for a clause that doesn't read: nothing of a
    /// count superseded is started.
    #[test]
    fn a_stopped_count_gives_nothing() {
        let ds = layer(3 * CHUNK);
        assert_eq!(count("n >= 0", &ds, &AtomicBool::new(true)), None);
        assert_eq!(count("nope = 1", &ds, &AtomicBool::new(true)), None, "not even parsed");
        assert_eq!(count("n >= 0", &ds, &AtomicBool::new(false)), Some(Answer::Rows(3 * CHUNK)));
    }

    /// A stop set while it counts ends it at the next chunk, with no answer.
    #[test]
    fn a_count_stopped_while_it_runs_ends_at_the_next_chunk() {
        let ds = layer(3 * CHUNK);
        let stop = AtomicBool::new(false);
        let mut chunks = 0;
        let answer = count_between("n >= 0", &ds, &stop, || {
            chunks += 1;
            stop.store(true, Ordering::Relaxed);
        });
        assert_eq!((answer, chunks), (None, 1), "one chunk of three");
    }

    /// A bug in the count is an answer, not a thread that ends with none.
    #[test]
    fn a_panic_in_the_count_is_an_answer() {
        let answer = guarded(|| panic!("a bug in the count"));
        assert!(matches!(answer, Some(Answer::Failed(_))), "{answer:?}");
        assert_eq!(guarded(|| Some(Answer::Rows(3))), Some(Answer::Rows(3)), "an answer passes through");
    }

    /// A thread the system refuses is said, not counted on the frame's thread (a large layer would
    /// freeze the window), and not called invalid or left counting.
    #[test]
    fn a_refused_thread_is_said_not_counted_here() {
        let ds = layer(100);
        let mut c = Count::default();
        c.refuse = true;
        let ctx = egui::Context::default();
        let shown = c.of(&ctx, "n >= 90", "t", &ds, true);
        assert!(matches!(&shown, Shown::Failed(e) if e.contains("no threads left")), "{shown:?}");
        assert!(!c.counting());
        assert_eq!(c.of(&ctx, "n >= 90", "t", &ds, true), shown, "kept");
        assert_eq!(c.starts, 1);
    }

    /// On a thread of its own the answer lands and the next ask shows it.
    #[test]
    fn a_count_on_a_thread_lands() {
        let ds = layer(100);
        let mut c = Count::default();
        let ctx = egui::Context::default();
        assert_eq!(c.of(&ctx, "n >= 90", "t", &ds, true), Shown::Counting, "not in the frame that starts it");
        let mut shown = Shown::Counting;
        for _ in 0..400 {
            shown = c.of(&ctx, "n >= 90", "t", &ds, true);
            if shown != Shown::Counting {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5)); // test-wait: the count runs on a thread of its own
        }
        assert_eq!(shown, Shown::Rows(10));
        assert_eq!(c.starts, 1);
    }

    /// The browser's path counts at once, on the first ask, and starts no thread.
    #[test]
    fn the_browser_counts_inline() {
        let ds = layer(100);
        let mut c = Count::default();
        let ctx = egui::Context::default();
        assert_eq!(c.of(&ctx, "n >= 90", "t", &ds, false), Shown::Rows(10));
        assert!(!c.counting());
        assert_eq!(c.starts, 1);
        assert_eq!(c.of(&ctx, "n >= 90", "t", &ds, false), Shown::Rows(10), "kept");
        assert_eq!(c.starts, 1, "not counted again");
    }

    /// Dropping the count stops one on its way.
    #[test]
    fn dropping_the_count_stops_it() {
        let c = Count::default();
        let stop = c.stop.clone();
        assert!(!stop.load(Ordering::Relaxed));
        drop(c);
        assert!(stop.load(Ordering::Relaxed));
    }

    /// Edited rows, another clause or another layer's name are counted again, not answered with
    /// the number for the rows before.
    #[test]
    fn rows_clause_and_layer_changed_are_counted_again() {
        let mut ds = layer(100);
        let mut c = Count::default();
        let ctx = egui::Context::default();
        assert_eq!(c.of(&ctx, "n >= 90", "t", &ds, false), Shown::Rows(10));
        ds.push(None, vec![Value::Int(500)]);
        assert_eq!(c.of(&ctx, "n >= 90", "t", &ds, false), Shown::Rows(11), "a row added");
        assert_eq!(c.of(&ctx, "n >= 95", "t", &ds, false), Shown::Rows(6), "another clause");
        assert_eq!(c.of(&ctx, "n >= 95", "u", &ds, false), Shown::Rows(6), "another layer");
        assert_eq!(c.starts, 4);
        assert_eq!(
            c.of(&ctx, "n >", "u", &ds, false),
            Shown::Invalid("expected a value, found the end".into()),
            "no count for a clause that doesn't read"
        );
        assert_eq!(c.starts, 4);
        // A value changed in place keeps the length and the fields: still counted again.
        let mut c = Count::default();
        assert_eq!(c.of(&ctx, "n >= 90", "t", &ds, false), Shown::Rows(11));
        ds.features.set_value(0, 0, Value::Int(95));
        assert_eq!(c.of(&ctx, "n >= 90", "t", &ds, false), Shown::Rows(12), "a value edited in place");
        assert_eq!(c.starts, 2);
    }
}
