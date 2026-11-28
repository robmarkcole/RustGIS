//! A plugin command's `cube.reduce` or `cube.trend` on a STAC collection, run by the app, reads
//! its slices off the frame (#3183), as a download the command leaves to the app does (#1202).
//!
//! The call checks its parameters and searches the collection as before, then answers
//! `{"pending": true}` and leaves the read to the app: whatever the command did is undone
//! quietly, the read runs on a thread of its own (its progress and Cancel in the status bar,
//! naming the plugin's command), and the command runs again from the start once it has landed.
//! That run's call, with the same parameters, adds what was read at once. Several in one call are
//! read one after another, and the command runs again once all have landed. Cancel, or a read that
//! failed, is that call's answer in the run again, and the plugin says it as it says any error.
//!
//! The read's thread also makes the layers (their GeoTIFFs written), and the run again adds those
//! very layers, sharing their pixels: what was read is held once, counted in what the app holds
//! ([`memory::held`]), until no command waits for it. A new read is refused when the reads waiting
//! would hold more than the memory the app plans for ([`memory::share`]), or more than
//! [`MAX_SUMMARIES`] would wait, landed ones included.
//!
//! Only a plugin's command the app runs ([`Session::run_leaving_downloads`]) leaves the read: the
//! command line, scripts, the assistant and Python wait for it, as before, and layers, bands and
//! Zarr stores, quick to read, are read where the call is made.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rustgis_data::memory;
use rustgis_data::raster::Raster;
use rustgis_map::raster::RasterLayer;
use serde_json::{Value, json};

use crate::render::rustgis_tiles::cache;
use crate::{CmdResult, Session};

/// The most summaries plugin commands may have left to read at once, being read or landed: each
/// holds its layers until its command has run again.
pub const MAX_SUMMARIES: usize = 4;

/// The key a summary's request names its command by (`{"summary": "cube.reduce", "params": …}`),
/// which no download's request has with no address.
const SUMMARY: &str = "summary";

/// What a summary's thread does: the slices read and folded, `done` counting them and `cancel`
/// stopping it between them.
type Run = Box<dyn FnOnce(&AtomicBool, &AtomicUsize) -> Result<Vec<Raster>, String> + Send>;

/// A summary (or a trend) prepared on the session: it reads nothing more of it, so it runs on any
/// thread.
pub(crate) struct Work {
    total: usize,
    dates: usize,
    /// About the bytes its layers will hold, planned before it starts.
    bytes: u64,
    run: Run,
}

impl Work {
    /// `run`, `total` steps of work, of `dates` dates, its layers holding about `bytes`.
    pub(crate) fn new(
        total: usize,
        dates: usize,
        bytes: u64,
        run: impl FnOnce(&AtomicBool, &AtomicUsize) -> Result<Vec<Raster>, String> + Send + 'static,
    ) -> Work {
        Work { total, dates, bytes, run: Box::new(run) }
    }
}

/// A summary the plugin call running now left, held until the call is kept to run again.
pub(crate) struct Left {
    n: u64,
    command: &'static str,
    work: Work,
    /// The call's request context, which the reads on the thread keep: they stop once its plugin
    /// or project is gone, and a plugin's are held to the internet as its call's are.
    still: Option<cache::Still>,
    capture: rustgis_data::diagnostics::Capture,
}

impl Left {
    /// Its request's number.
    pub(crate) fn number(&self) -> u64 {
        self.n
    }
}

/// A summary being read off the frame, shared with its thread.
#[derive(Clone)]
pub(crate) struct Reading {
    n: u64,
    command: &'static str,
    /// The plugin command it is read for.
    owner: String,
    total: usize,
    /// About the bytes its layers will hold.
    bytes: u64,
    cancel: Arc<AtomicBool>,
    done: Arc<AtomicUsize>,
    slot: Arc<Mutex<Option<Result<Held, String>>>>,
}

/// What a summary's thread made: its layers (with their names) and how many dates, and its part of
/// what the app holds.
pub(crate) struct Held {
    layers: Vec<(String, RasterLayer)>,
    dates: usize,
    _holding: memory::Holding,
}

/// What a summary read off the frame made, held for its command's run again, which adds those
/// layers, sharing their pixels: a run waiting again, for another read, adds them again.
#[derive(Clone)]
pub(crate) struct Summary(Arc<Held>);

impl std::fmt::Debug for Summary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Summary({} layers of {} dates)", self.0.layers.len(), self.0.dates)
    }
}

/// `rasters` as the layers a summary adds, made on its thread, and the bytes they hold.
fn made(rasters: Vec<Raster>) -> Result<(Vec<(String, RasterLayer)>, u64), String> {
    let layers = rasters.into_iter().map(super::super::cube::output_layer).collect::<Result<Vec<_>, _>>()?;
    let bytes = layers
        .iter()
        .map(|(_, l)| {
            let pixels = l.data.as_ref().map_or(0, |r| r.bands.len() as u64 * u64::from(r.width) * u64::from(r.height) * 4);
            pixels + l.embedded.as_ref().map_or(0, |e| e.len() as u64)
        })
        .sum();
    Ok((layers, bytes))
}

impl PartialEq for Summary {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// A summary under way, as the status bar shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct UnderWay {
    pub id: u64,
    /// The plugin command it is read for (`plugin:<plugin>/<command>`).
    pub command: String,
    /// The slices read, and how many there are.
    pub done: usize,
    pub total: usize,
    pub cancelled: bool,
}

/// Whether a request asks for a summary read off the frame, rather than for a download or a
/// service's document (which have an address).
pub(crate) fn is_summary(q: &Value) -> bool {
    q.get("url").is_none() && matches!(q.get(SUMMARY).and_then(Value::as_str), Some("cube.reduce" | "cube.trend"))
}

/// The request for `command` with `p`.
fn asked(command: &str, p: &Value) -> Value {
    json!({SUMMARY: command, "params": p})
}

/// Whether `command` with `p` leaves its read off the frame: a plugin's call the app runs (on the
/// desktop) that reads a STAC collection, by itself or through a catalogue's entry.
fn defers(s: &Session, p: &Value) -> bool {
    let given = |p: &Value, k: &str| p.get(k).is_some_and(|v| !v.is_null());
    cfg!(not(target_arch = "wasm32"))
        && s.leave_downloads
        && s.call_requests.is_some()
        && crate::cmd::cube_catalog::with_entry(p).is_ok_and(|p| given(&p, "collection") && !given(&p, "layers"))
}

/// `command` with `p` (`cube.reduce`, `cube.trend`) when its read is left off the frame
/// ([`defers`]): what was read, added, in the command's run again; else `{"pending": true}`, the
/// read prepared by `start` to begin once the call is kept. `None` when it is read here.
pub(crate) fn left(
    s: &mut Session,
    command: &'static str,
    p: &Value,
    start: impl FnOnce(&mut Session) -> Result<Work, String>,
) -> Option<CmdResult> {
    defers(s, p).then(|| s.left_summary(command, p, start))
}

/// The cancelled read's error, which its command's call answers.
fn cancelled(command: &str) -> String {
    format!("{command} was cancelled")
}

/// One summary read at a time: each takes the threads the app reads and computes with.
static TURN: Mutex<()> = Mutex::new(());

/// Tests: holds the summaries back, so one is cancelled before it starts.
#[cfg(test)]
pub(crate) fn hold_turn() -> std::sync::MutexGuard<'static, ()> {
    TURN.lock().unwrap_or_else(|e| e.into_inner())
}

impl Session {
    fn left_summary(&mut self, command: &'static str, p: &Value, start: impl FnOnce(&mut Session) -> Result<Work, String>) -> CmdResult {
        let asked = asked(command, p);
        if let Some((answer, added)) = self.rerun_answer(&asked) {
            let held = answer.and_then(|_| added.summary.ok_or_else(|| format!("{command}: nothing was read")))?;
            return super::super::cube::add_output_layers(self, &held.0.layers, held.0.dates);
        }
        // Asked for already in this call, or by a run before it for the same caller: that read,
        // not another (as a document's is, `left_document`).
        let caller = self.continuation().ok();
        let owns = |n: u64| {
            caller.as_ref().is_some_and(|c| {
                self.url_waits.owner(n).is_some_and(|owner| self.same_url_caller(owner, c) && self.check_continuation(owner).is_ok())
            })
        };
        let now = self.call_requests.as_deref().unwrap_or_default();
        let known = now
            .iter()
            .find(|(_, q)| super::fetch::same_request(q, &asked))
            .map(|(n, _)| *n)
            .or_else(|| self.url_waits.in_flight(&asked, owns));
        let number = match known {
            Some(n) => n,
            None => {
                // Those being read, those landed whose commands haven't run again, and this call's
                // new ones: each counted once.
                let waiting = self.url_waits.summaries.len() + self.url_waits.held_summaries() + self.call_summaries.len();
                if waiting >= MAX_SUMMARIES {
                    return Err(format!(
                        "{MAX_SUMMARIES} data cube summaries are already being read for plugin commands; try again when they have come"
                    ));
                }
                let work = start(self)?;
                // What those still being read will hold, with this one: those made or landed are
                // counted in what the app holds already.
                let (reading, held) = self.url_waits.reading_and_held();
                let planned = work.bytes + reading + self.call_summaries.iter().map(|l| l.work.bytes).sum::<u64>();
                if let Some(room) = memory::share(memory::physical(), held, memory::available())
                    && planned > room
                {
                    let mb = |b: u64| b.div_ceil(1 << 20);
                    return Err(format!(
                        "{command}: the summaries plugin commands are reading would hold about {} MB until their commands run again, more than the {} MB there is room for: take a smaller area or a coarser cell size, or try again once the reads under way have come",
                        mb(planned),
                        mb(room)
                    ));
                }
                self.next_request += 1;
                let n = self.next_request;
                let (still, capture) = (cache::current_still(), rustgis_data::diagnostics::Capture::current());
                self.call_summaries.push(Left { n, command, work, still, capture });
                n
            }
        };
        if let Some(asked_now) = self.call_requests.as_mut()
            && !asked_now.iter().any(|(n, _)| *n == number)
        {
            asked_now.push((number, asked));
        }
        Ok(json!({"pending": true}))
    }

    /// The summaries plugin commands left to read off the frame, under way.
    pub fn plugin_summaries(&self) -> Vec<UnderWay> {
        self.url_waits
            .summaries
            .iter()
            .map(|r| UnderWay {
                id: r.n,
                command: r.owner.clone(),
                done: r.done.load(Ordering::Relaxed).min(r.total),
                total: r.total,
                cancelled: r.cancel.load(Ordering::Relaxed),
            })
            .collect()
    }

    /// Stops summary `id` between two slices: its command runs again, its call answering that it
    /// was cancelled.
    pub fn cancel_plugin_summary(&mut self, id: u64) {
        if let Some(r) = self.url_waits.summaries.iter().find(|r| r.n == id) {
            r.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Tests in other crates: a summary under way for plugin command `owner`, read until it is
    /// cancelled (one of its 4 slices read), and its number. Only in a test build.
    #[cfg(any(test, all(feature = "testing", debug_assertions)))]
    #[doc(hidden)]
    pub fn start_test_summary(&mut self, owner: &str) -> u64 {
        let work = Work::new(4, 4, 0, |cancel: &AtomicBool, done: &AtomicUsize| {
            done.store(1, Ordering::Relaxed);
            while !cancel.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(5)); // test-wait: a read that lasts until it is cancelled
            }
            Ok(Vec::new())
        });
        self.next_request += 1;
        let n = self.next_request;
        self.url_waits.read_summary(
            Left { n, command: "cube.reduce", work, still: None, capture: rustgis_data::diagnostics::Capture::current() },
            owner,
        );
        n
    }
}

impl super::fetch::UrlWaits {
    /// Starts reading `left` off the frame for plugin command `owner`, after the summaries
    /// started before it.
    pub(crate) fn read_summary(&mut self, left: Left, owner: &str) {
        let Left { n, command, work, still, capture } = left;
        let Work { total, dates, bytes, run } = work;
        let reading = Reading {
            n,
            command,
            owner: owner.to_string(),
            total: total.max(1),
            bytes,
            cancel: Arc::default(),
            done: Arc::default(),
            slot: Arc::default(),
        };
        let (cancel, done, slot) = (reading.cancel.clone(), reading.done.clone(), reading.slot.clone());
        let spawned = std::thread::Builder::new().name("cube summary".into()).spawn(move || {
            let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
            let out = if cancel.load(Ordering::Relaxed) {
                Err(cancelled(command))
            } else {
                // A bug in the read, as one in a tool, must not take the app down.
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    cache::with_optional_still(still, || {
                        let _diagnostics = capture.enter();
                        run(&cancel, &done)
                    })
                }))
                .unwrap_or_else(|_| Err(format!("{command} failed unexpectedly")));
                if cancel.load(Ordering::Relaxed) { Err(cancelled(command)) } else { r }
            };
            // Its layers made here too, their GeoTIFFs written: the run again only adds them.
            let out = out.and_then(made);
            // Counted in what the app holds as they are put in the slot, under its lock: a new
            // read's plan finds them there or still to come, never both (`reading_and_held`).
            let mut slot = slot.lock().unwrap_or_else(|e| e.into_inner());
            *slot = Some(out.map(|(layers, bytes)| Held { layers, dates, _holding: memory::Holding::of(bytes) }));
        });
        if let Err(e) = spawned {
            *reading.slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(Err(format!("{command} couldn't start: {e}")));
        }
        self.summaries.push(reading);
    }

    /// Lands the summaries read by now. One whose command gave up is dropped, and forgotten.
    pub(super) fn summaries_in(&mut self) {
        for r in std::mem::take(&mut self.summaries) {
            let out = r.slot.lock().unwrap_or_else(|e| e.into_inner()).take();
            let Some(out) = out else {
                self.summaries.push(r);
                continue;
            };
            // Cancelled after its thread had finished, it is cancelled all the same.
            let out = if r.cancel.load(Ordering::Relaxed) { Err(cancelled(r.command)) } else { out };
            match out {
                Ok(held) => {
                    self.land(r.n, Ok(json!({})), super::fetch::Added::of_summary(Summary(Arc::new(held))));
                }
                Err(e) => self.land(r.n, Err(e), super::fetch::Added::default()),
            }
            self.refused(r.n);
        }
    }

    /// What the summaries still being read will hold, by what each planned, and what the app holds
    /// now, read together under their slots' locks: one whose layers are made is counted in what
    /// the app holds, and not again among those still being read.
    pub(super) fn reading_and_held(&self) -> (u64, u64) {
        let slots: Vec<_> = self.summaries.iter().map(|r| (r.bytes, r.slot.lock().unwrap_or_else(|e| e.into_inner()))).collect();
        let reading = slots.iter().filter(|(_, slot)| slot.is_none()).map(|(bytes, _)| bytes).sum();
        (reading, memory::held())
    }

    /// How many summaries have landed whose commands haven't run again: each holds its layers.
    fn held_summaries(&self) -> usize {
        self.landed.iter().filter(|(.., added)| added.summary.is_some()).count()
    }

    /// Stops every summary being read: their project is gone (replaced, or closed).
    pub(crate) fn stop_every_summary(&self) {
        for r in &self.summaries {
            r.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Stops the summaries being read that `kept` doesn't have: a failed command's, as its waits
    /// go back to `kept`, from before it.
    pub(crate) fn stop_summaries_beside(&self, kept: &Self) {
        for r in self.summaries.iter().filter(|r| !kept.summarising(r.n)) {
            r.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Whether request `n` is a summary still being read: it isn't given up for taking long.
    pub(super) fn summarising(&self, n: u64) -> bool {
        self.summaries.iter().any(|r| r.n == n)
    }

    /// Stops and forgets the summaries no command waits for any more (`wanted`).
    pub(super) fn stop_summaries(&mut self, wanted: impl Fn(u64) -> bool) {
        self.summaries.retain(|r| {
            let keep = wanted(r.n);
            if !keep {
                r.cancel.store(true, Ordering::Relaxed);
            }
            keep
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A summary's request has no address, so no front end's download lands it, and a download's
    /// request, whatever it carries, isn't one.
    #[test]
    fn a_summary_is_no_download() {
        let q = asked("cube.reduce", &json!({"collection": "s2"}));
        assert!(is_summary(&q) && is_summary(&asked("cube.trend", &json!({}))));
        assert!(!is_summary(&json!({"url": "https://x.org/a.csv", "summary": "cube.reduce"})));
        assert!(!is_summary(&json!({"summary": "layer.addUrl"})));
        let mut waits = super::super::fetch::UrlWaits::default();
        waits.wait(7, q);
        assert!(!waits.holds(7, &json!({"url": "https://x.org/a.csv", "request": 7})));
        assert!(!waits.holds(7, &json!({"request": 7})));
    }

    /// Cancelled once its thread had finished, before it landed, a read lands as cancelled: its
    /// command adds nothing.
    #[test]
    fn a_cancel_after_the_read_ended_is_its_answer() {
        let mut waits = super::super::fetch::UrlWaits::default();
        waits.wait(1, asked("cube.reduce", &json!({"n": "ended"})));
        let work = Work::new(1, 2, 0, |_: &AtomicBool, _: &AtomicUsize| Ok(Vec::new()));
        waits.read_summary(
            Left { n: 1, command: "cube.reduce", work, still: None, capture: rustgis_data::diagnostics::Capture::current() },
            "plugin:p/f",
        );
        let started = web_time::Instant::now();
        while waits.summaries[0].slot.lock().unwrap().is_none() && started.elapsed() < web_time::Duration::from_secs(30) {
            std::thread::sleep(std::time::Duration::from_millis(5)); // test-wait: the read ends on its thread
        }
        waits.summaries[0].cancel.store(true, Ordering::Relaxed);
        waits.summaries_in();
        let landed = waits.landed.iter().find(|(m, ..)| *m == 1).map(|(_, _, answer, _)| answer.clone());
        assert_eq!(landed, Some(Err("cube.reduce was cancelled".to_string())));
    }

    /// A read whose layers are made is counted in what the app holds, and no more among those still
    /// to hold theirs: a new read's plan counts it once.
    #[test]
    fn a_read_made_is_counted_once() {
        let mut waits = super::super::fetch::UrlWaits::default();
        let raster = || {
            let t = rustgis_data::raster::GeoTransform { origin_x: 10.0, origin_y: 46.0, pixel_width: 0.25, pixel_height: -0.25 };
            Raster::new(
                "r",
                4,
                4,
                vec![vec![1.0; 16]],
                t,
                rustgis_data::raster::Crs::LonLat,
                None,
                rustgis_data::raster::ColorHint::Gray,
                "Float32",
            )
            .unwrap()
        };
        let left = |n, work| Left { n, command: "cube.reduce", work, still: None, capture: rustgis_data::diagnostics::Capture::current() };
        waits.wait(1, asked("cube.reduce", &json!({"n": "made"})));
        waits.read_summary(left(1, Work::new(1, 2, 1000, move |_: &AtomicBool, _: &AtomicUsize| Ok(vec![raster()]))), "plugin:p/f");
        let started = web_time::Instant::now();
        while waits.summaries[0].slot.lock().unwrap().is_none() && started.elapsed() < web_time::Duration::from_secs(30) {
            std::thread::sleep(std::time::Duration::from_millis(5)); // test-wait: the read ends on its thread
        }
        assert_eq!(waits.reading_and_held().0, 0, "made: in what the app holds");
        // One that hasn't started is counted by what it planned.
        let turn = hold_turn();
        waits.wait(2, asked("cube.reduce", &json!({"n": "waits"})));
        waits.read_summary(left(2, Work::new(1, 2, 700, |_: &AtomicBool, _: &AtomicUsize| Ok(Vec::new()))), "plugin:p/f");
        assert_eq!(waits.reading_and_held().0, 700);
        drop(turn);
    }

    /// A summary read off the frame lands for the command waiting for it, a cancelled one as its
    /// error; one no command waits for any more is stopped.
    #[test]
    fn summaries_land_or_are_stopped() {
        let mut waits = super::super::fetch::UrlWaits::default();
        let work = |rasters: Vec<Raster>| {
            Work::new(2, 2, 0, move |_: &AtomicBool, done: &AtomicUsize| {
                done.fetch_add(2, Ordering::Relaxed);
                Ok(rasters)
            })
        };
        let left = |n, work| Left { n, command: "cube.reduce", work, still: None, capture: rustgis_data::diagnostics::Capture::current() };
        for n in 1..=3 {
            waits.wait(n, asked("cube.reduce", &json!({"n": n})));
        }
        // The third waits for the second's turn, which waits for the test's.
        let turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
        waits.read_summary(left(1, work(Vec::new())), "plugin:p/f");
        waits.read_summary(left(2, work(Vec::new())), "plugin:p/f");
        waits.read_summary(left(3, work(Vec::new())), "plugin:p/g");
        waits.summaries.iter().find(|r| r.n == 2).unwrap().cancel.store(true, Ordering::Relaxed);
        waits.stop_summaries(|n| n != 3);
        assert_eq!(waits.summaries.len(), 2);
        drop(turn);
        let started = web_time::Instant::now();
        while !waits.summaries.is_empty() && started.elapsed() < web_time::Duration::from_secs(30) {
            waits.summaries_in();
            std::thread::sleep(std::time::Duration::from_millis(5)); // test-wait: the reads land on their threads, polled as the frame loop polls them
        }
        let landed = |n: u64| waits.landed.iter().find(|(m, ..)| *m == n).map(|(_, _, answer, added)| (answer.clone(), added.clone()));
        assert!(landed(1).is_some_and(|(answer, added)| answer.is_ok() && added.summary.is_some()));
        assert_eq!(landed(2).map(|(answer, _)| answer), Some(Err("cube.reduce was cancelled".to_string())));
        assert!(landed(3).is_none(), "stopped, and not landed");
    }
}
