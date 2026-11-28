//! The copies of picked files a phone keeps (#1902). A file picked on a phone is copied into the
//! app's storage (`picked/<pick>/…`, by Android's activity and by the iOS shell), and data and
//! projects open from the copy and are edited there. Nothing removes a copy by itself: a project
//! saved on the phone names its data by the copies' paths, so a copy that looks unused may still be
//! wanted. Remove Unused Copies, in the More sheet, removes the picks the project in the window
//! doesn't name, once the user has seen how many and how much, and agreed.
//!
//! A pick's folder is kept or removed whole: a project names a Shapefile's `.shp`, not its `.dbf`,
//! nor a GeoPackage's `-wal`, so a pick is in use when the project names any file in it.
//!
//! A pick still being copied into is kept too. The phones mark a pick's folder with [`COPYING`]
//! while they copy into it, and write each copy under a `.part` name of its own, renamed once
//! whole. A mark owned by this process protects the whole batch regardless of its age. A mark
//! from a previous process, or a `.part` left after a crash, stops counting after [`STALE`].

use rustgis_ui_egui::rustgis_engine::data::dirs;
use rustgis_ui_egui::{RustGisApp, i18n, tl};
use std::collections::HashSet;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// The deepest a file of a pick is (`picked/<pick>/<source folder>/<file>`), looked for a level
/// deeper: a pick holds no deeper folders, and a link out of it is never followed.
const DEPTH: usize = 3;

/// The file the phones put in a pick's folder while they copy documents into it, for the whole
/// pick, and remove once done.
pub const COPYING: &str = ".copying";

/// The process lifetime token shared with the platform copy worker. Android installs Java's
/// static UUID before the phone is used; iOS uses the token generated here.
static COPY_OWNER: OnceLock<String> = OnceLock::new();

/// This process's document-copy token. It is independent of the wall clock and process-id reuse.
pub fn copy_owner() -> &'static str {
    use std::hash::BuildHasher as _;
    COPY_OWNER.get_or_init(|| {
        let random = std::collections::hash_map::RandomState::new();
        format!("process-{:016x}{:016x}", random.hash_one(0u8), random.hash_one(1u8))
    })
}

/// Installs Android's process-wide Java token before the front end can inspect unused copies.
/// A conflicting second installation is refused rather than treating another owner as active.
pub fn set_copy_owner(owner: String) -> Result<(), String> {
    if owner.is_empty() || owner.len() > 128 || !owner.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return Err(tl!("The document-copy owner is invalid.").to_string());
    }
    if let Some(current) = COPY_OWNER.get() {
        return if current == &owner { Ok(()) } else { Err(tl!("The document-copy owner was already initialized.").to_string()) };
    }
    COPY_OWNER.set(owner).map_err(|_| tl!("The document-copy owner was already initialized.").to_string())
}

/// A document batch's mark, removed on completion or unwinding. The platform keeps this guard
/// alive across document-provider waits and gaps between files.
pub struct Copying {
    mark: PathBuf,
}

/// Marks `folder` for this process for the guard's whole lifetime.
pub fn mark_copying(folder: &Path) -> std::io::Result<Copying> {
    std::fs::create_dir_all(folder)?;
    let mark = folder.join(COPYING);
    std::fs::write(&mark, copy_owner())?;
    Ok(Copying { mark })
}

impl Drop for Copying {
    fn drop(&mut self) {
        if owned_mark(&self.mark) {
            let _ = std::fs::remove_file(&self.mark);
        }
    }
}

/// Reads at most the token limit plus one byte, even for a damaged or hostile marker file.
fn owned_mark(file: &Path) -> bool {
    use std::io::Read as _;
    let Ok(file) = std::fs::File::open(file) else { return false };
    let mut owner = Vec::with_capacity(129);
    file.take(129).read_to_end(&mut owner).is_ok() && owner == copy_owner().as_bytes()
}

/// How long an abandoned or legacy [`COPYING`] mark or `.part` file counts after it was last written: a copy
/// writes its `.part` as it goes, but iOS may first download a document from iCloud for a while.
const STALE: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// The picks the project doesn't name.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Unused {
    /// Each pick's folder (or a loose file directly in `picked/`).
    pub picks: Vec<PathBuf>,
    /// The files in them.
    pub files: usize,
    /// Their size, in bytes.
    pub bytes: u64,
    /// Entries that couldn't be read in full, kept: what isn't seen can't be judged unused.
    pub unreadable: usize,
    /// The picks among them (an entry not even listed has no path).
    pub unreadable_picks: Vec<PathBuf>,
    /// Picks being copied into (or just made, with no file yet), kept.
    pub busy_picks: Vec<PathBuf>,
}

/// What Remove Them did.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Removed {
    pub files: usize,
    pub bytes: u64,
    /// What couldn't be removed, each in its own words.
    pub failed: Vec<String>,
    /// Picks found unused when asked that the project names now (it changed since), kept.
    pub now_named: usize,
    /// Picks found unused when asked that couldn't be read in full when looked at again, kept.
    pub unreadable: usize,
    /// Picks found unused when asked that are being copied into now, kept.
    pub busy: usize,
}

/// Where the copies are, where the app has a folder of its own (a phone).
pub(crate) fn folder() -> Option<PathBuf> {
    dirs::root().map(|root| root.join("picked"))
}

/// How the project is written out as JSON, as it is saved, to a writer.
pub(crate) type WriteProject<'a> = &'a dyn Fn(&mut dyn Write) -> Result<(), serde_json::Error>;

/// The picks in `picked` none of whose files the project names, looking for `cancelled` between
/// picks and as the project is written out. The project is scanned as it is written: only a
/// buffer of its text is held at a time, never all of it (see [`Names`]).
pub(crate) fn unused(picked: &Path, project: WriteProject, cancelled: &AtomicBool) -> Result<Unused, String> {
    let entries = std::fs::read_dir(picked).map_err(|e| format!("{}: {e}", picked.display()))?;
    let mut found = Unused::default();
    let mut picks = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => picks.push(entry.path()),
            // Not seen, so not judged: counted, and said.
            Err(_) => found.unreadable += 1,
        }
    }
    picks.sort();
    // The picks left to judge, with their files.
    let mut candidates = Vec::new();
    for pick in picks {
        if cancelled.load(Ordering::Relaxed) {
            return Err(tl!("Stopped.").to_string());
        }
        let mut files = Vec::new();
        // A pick not read in full may hold a file the project names: kept.
        if !gather(&pick, DEPTH, &mut files) {
            found.unreadable += 1;
            found.unreadable_picks.push(pick);
            continue;
        }
        // A pick with no file is being made, and one with a recent mark or `.part` file is being
        // copied into: kept.
        if files.is_empty() || files.iter().any(|(file, _)| in_flight(file)) {
            found.busy_picks.push(pick);
            continue;
        }
        candidates.push((pick, files));
    }
    // Nothing to judge: the project isn't written out at all (nor when no path is left to look for).
    if candidates.is_empty() {
        return Ok(found);
    }
    let prefix = escaped(&picked.to_string_lossy()).into_bytes();
    let rests: HashSet<Vec<u8>> = candidates.iter().flat_map(|(_, files)| files).filter_map(|(file, _)| rest_of(&prefix, file)).collect();
    // No file to look for (none of their paths can be told from the folder's): all are kept.
    let named = if rests.is_empty() { HashSet::new() } else { scan(project, Names::new(prefix.clone(), rests, cancelled))? };
    for (pick, files) in candidates {
        // A file whose path can't be told apart from the folder's is kept, never judged unused.
        if files.iter().any(|(file, _)| rest_of(&prefix, file).is_none_or(|rest| named.contains(&rest))) {
            continue;
        }
        found.files += files.len();
        found.bytes += files.iter().map(|(_, size)| size).sum::<u64>();
        found.picks.push(pick);
    }
    Ok(found)
}

/// The files at or under `path` (itself, if it is one), with their sizes, `depth` levels down at
/// most. Links aren't followed. Returns whether everything there could be read.
fn gather(path: &Path, depth: usize, files: &mut Vec<(PathBuf, u64)>) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else { return false };
    if meta.is_file() {
        files.push((path.to_path_buf(), meta.len()));
        return true;
    }
    // A link, or another entry that is neither file nor folder, isn't looked through, and a
    // folder too deep isn't looked into: not read in full, so its pick is kept.
    if !meta.is_dir() || depth == 0 {
        return false;
    }
    let Ok(entries) = std::fs::read_dir(path) else { return false };
    let mut complete = true;
    for entry in entries {
        match entry {
            Ok(entry) => complete &= gather(&entry.path(), depth - 1, files),
            Err(_) => complete = false,
        }
    }
    complete
}

/// A mark owned by this process, however old; otherwise a legacy/abandoned mark or `.part`
/// copy written in the last [`STALE`].
fn in_flight(file: &Path) -> bool {
    let mark = file.file_name().is_some_and(|n| n == COPYING);
    (mark && owned_mark(file)) || ((mark || file.extension().is_some_and(|e| e == "part")) && recent(file))
}

/// Whether `file` was last written in the last [`STALE`] (or in the future, as a clock set back
/// makes it).
fn recent(file: &Path) -> bool {
    let Ok(written) = std::fs::symlink_metadata(file).and_then(|m| m.modified()) else { return false };
    std::time::SystemTime::now().duration_since(written).map_or(true, |age| age < STALE)
}

/// `text` as JSON writes it inside a string, without the quotes.
fn escaped(text: &str) -> String {
    let quoted = serde_json::to_string(text).unwrap_or_default();
    quoted.strip_prefix('"').and_then(|q| q.strip_suffix('"')).unwrap_or(&quoted).to_string()
}

/// What follows `prefix` (the folder of the picks, [`escaped`]) in the escaped path of `file`: the
/// project names `file` when its JSON holds the folder followed by this. `None` when there is
/// nothing to look for, so the file is kept.
fn rest_of(prefix: &[u8], file: &Path) -> Option<Vec<u8>> {
    if prefix.is_empty() {
        return None;
    }
    let text = escaped(&file.to_string_lossy());
    text.as_bytes().strip_prefix(prefix).filter(|rest| !rest.is_empty()).map(<[u8]>::to_vec)
}

/// How much of the project's JSON is handed over at a time: what the scan looks at in one go.
const WRITE: usize = 64 * 1024;

/// Finds the files the project names in its JSON as it is written to it, holding none of that
/// text but a buffer of it at a time. A file is named when its path, written as JSON writes it, is anywhere in the text. Every
/// such path starts with the folder of the picks, so the scan looks for that alone (a byte search,
/// linear in the text) and, where it finds it, asks whether what follows is the rest of a path it
/// is looking for. What it keeps is the end of what it was given, as long as a path can be cut
/// in two writes: the folder and the longest rest, together; and one string for each file found.
/// A write larger than [`WRITE`] is taken that much at a time, and Cancel is looked at before
/// each. Once every path looked for is found it refuses the rest of the text, which ends the
/// writing.
struct Names<'a> {
    prefix: Vec<u8>,
    /// What follows the folder in each path looked for.
    rests: HashSet<Vec<u8>>,
    /// The distinct lengths of `rests`, to look up what follows a hit at each.
    lengths: Vec<usize>,
    /// The longest rest: a hit with that much after it, or at the end of the text, is settled.
    reach: usize,
    /// The rests found.
    found: HashSet<Vec<u8>>,
    /// The end of the text so far: from the first hit not yet settled, or what could begin one.
    carry: Vec<u8>,
    cancelled: &'a AtomicBool,
}

impl<'a> Names<'a> {
    fn new(prefix: Vec<u8>, rests: HashSet<Vec<u8>>, cancelled: &'a AtomicBool) -> Self {
        let mut lengths: Vec<usize> = rests.iter().map(Vec::len).collect();
        lengths.sort_unstable();
        lengths.dedup();
        let reach = lengths.last().copied().unwrap_or(0);
        Names { prefix, rests, lengths, reach, found: HashSet::new(), carry: Vec::new(), cancelled }
    }

    /// Settles the hits of the folder in `carry` that have all they need after them (all of them,
    /// once `last`: the text is over), and drops the text before the first that hasn't, or that
    /// a folder cut at the end could begin.
    fn look(&mut self, last: bool) {
        let (len, plen) = (self.carry.len(), self.prefix.len());
        let Some(&first) = self.prefix.first() else {
            self.carry.clear();
            return;
        };
        // A whole folder doesn't begin later than this; a part of it may.
        let mut keep = len.saturating_sub(plen.saturating_sub(1));
        let mut from = 0;
        while let Some(rest) = self.carry.get(from..)
            && let Some(offset) = rest.iter().position(|&b| b == first)
        {
            let at = from + offset;
            from = at + 1;
            let Some(after) = self.carry.get(at..).and_then(|text| text.strip_prefix(self.prefix.as_slice())) else { continue };
            if after.len() < self.reach && !last {
                keep = keep.min(at);
                break;
            }
            for &n in &self.lengths {
                if let Some(rest) = after.get(..n)
                    && self.rests.contains(rest)
                    && !self.found.contains(rest)
                {
                    self.found.insert(rest.to_vec());
                }
            }
        }
        if last {
            self.carry.clear();
        } else {
            self.carry.drain(..keep.min(len));
        }
    }
}

impl Names<'_> {
    /// Whether every path looked for has been found: the rest of the text can't change the answer.
    fn all_found(&self) -> bool {
        self.found.len() == self.rests.len()
    }
}

impl Write for Names<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        // A write as large as the `BufWriter`'s buffer (a thumbnail, a long text) comes whole:
        // taken a buffer at a time, so `carry` never holds more of it than that.
        for piece in buf.chunks(WRITE) {
            // Not `Interrupted`: `write_all` and `BufWriter` try again after that, for good.
            if self.cancelled.load(Ordering::Relaxed) || self.all_found() {
                return Err(std::io::Error::other("stopped"));
            }
            // A machine short of memory gets an error, not an abort.
            self.carry.try_reserve(piece.len()).map_err(|_| std::io::Error::other("out of memory"))?;
            self.carry.extend_from_slice(piece);
            self.look(false);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Writes the project out through `names` and returns the rests it found named. Stopped, if
/// `names`' `cancelled` was set while it wrote or by the end of it. The writing ends early, with
/// the answer, once every path looked for is found.
fn scan(project: WriteProject, names: Names) -> Result<HashSet<Vec<u8>>, String> {
    let cancelled = names.cancelled;
    let mut out = BufWriter::with_capacity(WRITE, names);
    let written = project(&mut out).map_err(|e| e.to_string()).and_then(|()| out.flush().map_err(|e| e.to_string()));
    // What the buffer still holds isn't needed once every path is found, and has been written
    // otherwise: taken out unwritten.
    let (mut names, _) = out.into_parts();
    if !names.all_found() {
        if cancelled.load(Ordering::Relaxed) {
            return Err(tl!("Stopped.").to_string());
        }
        written?;
    }
    names.look(true);
    Ok(names.found)
}

/// Removes the picks of `found` that `picked` still holds unused by `project` (the project as it
/// is now, looked at again: it may have changed since the user was asked), looking for
/// `cancelled` between them and as the project is written out.
fn remove_unused(picked: &Path, found: &Unused, project: WriteProject, cancelled: &AtomicBool) -> Result<Removed, String> {
    let now = unused(picked, project, cancelled)?;
    let still: Vec<PathBuf> = found.picks.iter().filter(|pick| now.picks.contains(pick)).cloned().collect();
    let mut removed = remove(&still, cancelled);
    // Each pick asked about that isn't removable now, counted once: not read in full, being copied
    // into, or named (what is left of those that still exist).
    let kept: Vec<&PathBuf> = found.picks.iter().filter(|pick| !now.picks.contains(pick)).collect();
    removed.unreadable = kept.iter().filter(|pick| now.unreadable_picks.contains(pick)).count();
    removed.busy = kept.iter().filter(|pick| now.busy_picks.contains(pick)).count();
    removed.now_named =
        kept.iter().filter(|pick| pick.exists() && !now.unreadable_picks.contains(pick) && !now.busy_picks.contains(pick)).count();
    Ok(removed)
}

/// Removes `picks` (folders or files), looking for `cancelled` between them.
fn remove(picks: &[PathBuf], cancelled: &AtomicBool) -> Removed {
    let (mut files, mut bytes, mut failed) = (0usize, 0u64, Vec::new());
    for pick in picks {
        if cancelled.load(Ordering::Relaxed) {
            break;
        }
        let mut held = Vec::new();
        gather(pick, DEPTH, &mut held);
        let gone = if pick.is_dir() { std::fs::remove_dir_all(pick) } else { std::fs::remove_file(pick) };
        // What is gone is counted, also when the removal stopped partway through a pick.
        let removed: Vec<&(PathBuf, u64)> =
            if gone.is_ok() { held.iter().collect() } else { held.iter().filter(|(file, _)| !file.exists()).collect() };
        files += removed.len();
        bytes += removed.iter().map(|(_, size)| size).sum::<u64>();
        if let Err(e) = gone {
            failed.push(format!("{}: {e}", pick.display()));
        }
    }
    Removed { files, bytes, failed, ..Removed::default() }
}

/// Where Remove Unused Copies is.
#[derive(Clone, Default)]
pub(crate) enum Job {
    #[default]
    Idle,
    /// Looking for the unused copies, off the window's thread.
    Scanning(Arc<Mutex<Option<Result<Unused, String>>>>, Arc<AtomicBool>),
    /// Found: waiting for the user to agree.
    Asking(Unused),
    /// Removing them, off the window's thread.
    Removing(Arc<Mutex<Option<Result<Removed, String>>>>, Arc<AtomicBool>),
}

fn id() -> egui::Id {
    egui::Id::unique("touch_copies")
}

pub(crate) fn job(ctx: &egui::Context) -> Job {
    ctx.data(|d| d.get_temp::<Job>(id())).unwrap_or_default()
}

fn set_job(ctx: &egui::Context, job: Job) {
    ctx.data_mut(|d| d.insert_temp(id(), job));
}

/// Starts looking for the copies the project doesn't name. The project is copied on the window's
/// thread (one step, without Cancel: its rows are shared, so only pointers are copied); it is
/// written out through the scan, which looks for Cancel as it goes, and the copies looked at off
/// it.
pub(crate) fn start(app: &mut RustGisApp, ctx: &egui::Context) {
    let Some(picked) = folder() else { return };
    let project = app.session.project.clone();
    let (result, cancelled) = (Arc::new(Mutex::new(None)), Arc::new(AtomicBool::new(false)));
    let (out, stop, wake) = (result.clone(), cancelled.clone(), ctx.clone());
    let started = std::thread::Builder::new().name("unused copies".into()).spawn(move || {
        let found = unused(&picked, &|out| serde_json::to_writer(out, &project), &stop);
        *out.lock().unwrap_or_else(|e| e.into_inner()) = Some(found);
        wake.request_repaint();
    });
    match started {
        Ok(_) => set_job(ctx, Job::Scanning(result, cancelled)),
        Err(e) => app.session.notify(i18n::fmt("The unused copies weren't looked for: {error}", &[("error", &e.to_string())]), true),
    }
}

/// Removes the copies the user agreed to, off the window's thread: those still unused by the
/// project as it is now (copied here, one step, its rows shared), as it may have changed while the
/// question was up (an Undo, another project opened).
pub(crate) fn confirm(app: &mut RustGisApp, ctx: &egui::Context, found: Unused) {
    let Some(picked) = folder() else {
        set_job(ctx, Job::Idle);
        return;
    };
    let project = app.session.project.clone();
    let (result, cancelled) = (Arc::new(Mutex::new(None)), Arc::new(AtomicBool::new(false)));
    let (out, stop, wake) = (result.clone(), cancelled.clone(), ctx.clone());
    let started = std::thread::Builder::new().name("removing copies".into()).spawn(move || {
        let removed = remove_unused(&picked, &found, &|out| serde_json::to_writer(out, &project), &stop);
        *out.lock().unwrap_or_else(|e| e.into_inner()) = Some(removed);
        wake.request_repaint();
    });
    match started {
        Ok(_) => set_job(ctx, Job::Removing(result, cancelled)),
        Err(e) => {
            set_job(ctx, Job::Idle);
            app.session.notify(i18n::fmt("The unused copies weren't removed: {error}", &[("error", &e.to_string())]), true);
        }
    }
}

/// Whether unused copies are being removed now. Done once the thread has finished, though its
/// answer is only said by the touch interface (which the user may have left meanwhile).
pub(crate) fn removing(ctx: &egui::Context) -> bool {
    match job(ctx) {
        Job::Removing(result, _) => result.lock().unwrap_or_else(|e| e.into_inner()).is_none(),
        _ => false,
    }
}

/// Lets go of what was found, or stops what is running.
pub(crate) fn cancel(ctx: &egui::Context) {
    match job(ctx) {
        Job::Scanning(_, stop) | Job::Removing(_, stop) => stop.store(true, Ordering::Relaxed),
        Job::Asking(_) | Job::Idle => set_job(ctx, Job::Idle),
    }
}

/// A size for people: megabytes with one decimal, or kilobytes under a tenth of a megabyte.
pub(crate) fn size(bytes: u64) -> String {
    let mb = bytes as f64 / (1024.0 * 1024.0);
    if mb >= 0.1 {
        i18n::fmt("{n} MB", &[("n", &format!("{mb:.1}"))])
    } else {
        i18n::fmt("{n} KB", &[("n", &bytes.div_ceil(1024).to_string())])
    }
}

/// Takes what the threads found, each frame: what is found is asked about, what was removed is
/// said.
pub(crate) fn poll(app: &mut RustGisApp, ctx: &egui::Context) {
    match job(ctx) {
        Job::Idle | Job::Asking(_) => {}
        Job::Scanning(result, stop) => match result.lock().unwrap_or_else(|e| e.into_inner()).take() {
            None => ctx.request_repaint_after(std::time::Duration::from_millis(250)),
            Some(Ok(found)) if found.picks.is_empty() => {
                set_job(ctx, Job::Idle);
                let mut text = tl!("No copies of picked files are unused: the project names every one.").to_string();
                if !found.busy_picks.is_empty() {
                    text.push(' ');
                    text.push_str(&busy_said(found.busy_picks.len()));
                }
                if found.unreadable > 0 {
                    text.push(' ');
                    text.push_str(&unreadable_said(found.unreadable));
                }
                app.session.notify(text, found.unreadable > 0);
            }
            Some(Ok(found)) => set_job(ctx, Job::Asking(found)),
            Some(Err(_)) if stop.load(Ordering::Relaxed) => set_job(ctx, Job::Idle),
            Some(Err(error)) => {
                set_job(ctx, Job::Idle);
                app.session.notify(i18n::fmt("The unused copies weren't looked for: {error}", &[("error", &error)]), true);
            }
        },
        Job::Removing(result, stop) => match result.lock().unwrap_or_else(|e| e.into_inner()).take() {
            None => ctx.request_repaint_after(std::time::Duration::from_millis(250)),
            Some(Err(_)) if stop.load(Ordering::Relaxed) => set_job(ctx, Job::Idle),
            Some(Err(error)) => {
                set_job(ctx, Job::Idle);
                app.session.notify(i18n::fmt("The unused copies weren't removed: {error}", &[("error", &error)]), true);
            }
            Some(Ok(removed)) => {
                set_job(ctx, Job::Idle);
                let (count, size) = (removed.files.to_string(), size(removed.bytes));
                let template = if removed.files == 1 {
                    "Removed 1 copy of a picked file ({size})."
                } else {
                    "Removed {count} copies of picked files ({size})."
                };
                let mut text = i18n::fmt(template, &[("count", &count), ("size", &size)]);
                let mut said = |more: String| {
                    text.push(' ');
                    text.push_str(&more);
                };
                if stop.load(Ordering::Relaxed) {
                    said(tl!("Stopped before the rest.").to_string());
                }
                if removed.now_named > 0 {
                    let n = removed.now_named.to_string();
                    said(i18n::fmt("{n} kept: the project names them now.", &[("n", &n)]));
                }
                if removed.busy > 0 {
                    said(busy_said(removed.busy));
                }
                if removed.unreadable > 0 {
                    said(unreadable_said(removed.unreadable));
                }
                if let Some(first) = removed.failed.first() {
                    let (n, first) = (removed.failed.len().to_string(), first.clone());
                    said(i18n::fmt("{n} couldn't be removed, the first: {first}", &[("n", &n), ("first", &first)]));
                }
                app.session.notify(text, !removed.failed.is_empty() || removed.unreadable > 0);
            }
        },
    }
}

/// What is said of `n` picks being copied into, kept.
fn busy_said(n: usize) -> String {
    i18n::fmt("{n} picks are still being copied into and were kept.", &[("n", &n.to_string())])
}

/// What is said of `n` picks that couldn't be read in full and were kept.
fn unreadable_said(n: usize) -> String {
    i18n::fmt("{n} picks couldn't be read in full and were kept.", &[("n", &n.to_string())])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A project written out as `value` is.
    fn of(value: serde_json::Value) -> impl Fn(&mut dyn Write) -> Result<(), serde_json::Error> {
        move |out| serde_json::to_writer(out, &value)
    }

    #[test]
    fn a_pick_is_kept_whole_when_the_project_names_any_file_in_it() {
        let root = std::env::temp_dir().join(format!("rustgis-copies-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let picked = root.join("picked");
        // A Shapefile the project names by its .shp (its .dbf beside it), and a GeoPackage it
        // doesn't name, from another pick; a loose file too. A path the project names is joined a
        // folder at a time, as the folder is listed: Windows lists `1\0` where `join("1/0")` keeps `1/0`.
        let shapes = picked.join("1").join("0");
        let unused_pick = picked.join("2").join("0");
        std::fs::create_dir_all(&shapes).unwrap();
        std::fs::create_dir_all(&unused_pick).unwrap();
        std::fs::write(shapes.join("roads.shp"), b"shp").unwrap();
        std::fs::write(shapes.join("roads.dbf"), b"dbf").unwrap();
        std::fs::write(unused_pick.join("parcels.gpkg"), b"0123456789").unwrap();
        std::fs::write(picked.join("loose.csv"), b"a,b").unwrap();
        let project = of(json!({"layers": [{"source": shapes.join("roads.shp").to_string_lossy()}]}));
        let found = unused(&picked, &project, &AtomicBool::new(false)).unwrap();
        assert_eq!(found.picks, vec![picked.join("2"), picked.join("loose.csv")]);
        assert_eq!((found.files, found.bytes), (2, 13));
        // Removed: the named pick stays, sidecar and all.
        let removed = remove(&found.picks, &AtomicBool::new(false));
        assert_eq!((removed.files, removed.bytes, removed.failed.len()), (2, 13, 0));
        assert!(shapes.join("roads.dbf").exists());
        assert!(!picked.join("2").exists() && !picked.join("loose.csv").exists());
        // Cancelled, nothing is looked at.
        assert!(unused(&picked, &project, &AtomicBool::new(true)).is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_pick_the_project_names_by_the_time_of_removal_is_kept() {
        let root = std::env::temp_dir().join(format!("rustgis-copies-late-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let picked = root.join("picked");
        for pick in ["1/0", "2/0"] {
            std::fs::create_dir_all(picked.join(pick)).unwrap();
        }
        let a = picked.join("1").join("0").join("a.gpkg");
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(picked.join("2").join("0").join("b.gpkg"), b"bb").unwrap();
        let found = unused(&picked, &of(json!({})), &AtomicBool::new(false)).unwrap();
        assert_eq!(found.picks.len(), 2);
        // An Undo brings back the layer reading a.gpkg before Remove Them.
        let now = of(json!({"path": a.to_string_lossy()}));
        let removed = remove_unused(&picked, &found, &now, &AtomicBool::new(false)).unwrap();
        assert_eq!((removed.files, removed.now_named), (1, 1));
        assert!(a.exists() && !picked.join("2").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_pick_with_a_copy_being_written_is_kept() {
        let root = std::env::temp_dir().join(format!("rustgis-copies-part-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let picked = root.join("picked");
        std::fs::create_dir_all(picked.join("4/0")).unwrap();
        std::fs::write(picked.join("4/0/big.gpkg.part"), b"half").unwrap();
        // Between two documents of one pick: a copy done, the next not started, the mark there.
        std::fs::create_dir_all(picked.join("5/0")).unwrap();
        std::fs::write(picked.join("5").join(COPYING), b"").unwrap();
        std::fs::write(picked.join("5/0/roads.gpkg"), b"whole").unwrap();
        let found = unused(&picked, &of(json!({})), &AtomicBool::new(false)).unwrap();
        assert!(found.picks.is_empty());
        assert_eq!(found.busy_picks, [picked.join("4"), picked.join("5")]);
        // Found unused when asked, then copied into before the removal: kept, and said as such.
        std::fs::remove_file(picked.join("5").join(COPYING)).unwrap();
        let asked = unused(&picked, &of(json!({})), &AtomicBool::new(false)).unwrap();
        assert_eq!(asked.picks, [picked.join("5")]);
        std::fs::write(picked.join("5/0/rivers.gpkg.part"), b"half").unwrap();
        let removed = remove_unused(&picked, &asked, &of(json!({})), &AtomicBool::new(false)).unwrap();
        assert_eq!((removed.files, removed.busy, removed.now_named), (0, 1, 0));
        assert!(picked.join("5/0/roads.gpkg").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn an_old_active_batch_between_documents_stays_protected_until_completion() {
        let root = std::env::temp_dir().join(format!("rustgis-copies-owner-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let picked = root.join("picked");
        let pick = picked.join("7");
        std::fs::create_dir_all(pick.join("0")).unwrap();
        std::fs::write(pick.join("0/roads.gpkg"), b"whole").unwrap();
        let asked = unused(&picked, &of(json!({})), &AtomicBool::new(false)).unwrap();
        let guard = mark_copying(&pick).unwrap();
        let mark = pick.join(COPYING);
        let long_ago = std::time::SystemTime::now() - STALE - std::time::Duration::from_secs(60);
        std::fs::File::options().write(true).open(&mark).unwrap().set_modified(long_ago).unwrap();
        // No .part exists: a provider is paused before the next document begins.
        let found = unused(&picked, &of(json!({})), &AtomicBool::new(false)).unwrap();
        assert!(found.picks.is_empty());
        assert_eq!(found.busy_picks.as_slice(), std::slice::from_ref(&pick));
        assert_eq!(remove_unused(&picked, &asked, &of(json!({})), &AtomicBool::new(false)).unwrap().busy, 1);
        assert!(pick.join("0/roads.gpkg").exists());
        drop(guard);
        assert!(!mark.exists());
        assert_eq!(unused(&picked, &of(json!({})), &AtomicBool::new(false)).unwrap().picks.as_slice(), std::slice::from_ref(&pick));
        std::fs::write(&mark, b"previous-process-owner").unwrap();
        std::fs::File::options().write(true).open(&mark).unwrap().set_modified(long_ago).unwrap();
        assert_eq!(unused(&picked, &of(json!({})), &AtomicBool::new(false)).unwrap().picks, [pick]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_pick_left_by_a_copy_cut_short_long_ago_is_judged_like_any_other() {
        let root = std::env::temp_dir().join(format!("rustgis-copies-stale-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let picked = root.join("picked");
        std::fs::create_dir_all(picked.join("6/0")).unwrap();
        let (mark, part) = (picked.join("6").join(COPYING), picked.join("6/0/big.gpkg.part"));
        std::fs::write(&mark, b"").unwrap();
        std::fs::write(&part, b"half").unwrap();
        let long_ago = std::time::SystemTime::now() - STALE - std::time::Duration::from_secs(60);
        for file in [&mark, &part] {
            std::fs::File::options().write(true).open(file).unwrap().set_modified(long_ago).unwrap();
        }
        let found = unused(&picked, &of(json!({})), &AtomicBool::new(false)).unwrap();
        assert_eq!(found.picks, [picked.join("6")]);
        assert!(found.busy_picks.is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_pick_with_no_file_is_kept() {
        let root = std::env::temp_dir().join(format!("rustgis-copies-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let picked = root.join("picked");
        std::fs::create_dir_all(picked.join("3/0")).unwrap();
        let found = unused(&picked, &of(json!({})), &AtomicBool::new(false)).unwrap();
        assert!(found.picks.is_empty());
        assert!(picked.join("3/0").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_pick_deeper_than_looked_at_is_kept() {
        let root = std::env::temp_dir().join(format!("rustgis-copies-deep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let picked = root.join("picked");
        let deep = picked.join("1/0/x/y");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("z.gpkg"), b"z").unwrap();
        let found = unused(&picked, &of(json!({})), &AtomicBool::new(false)).unwrap();
        assert_eq!((found.picks.len(), found.unreadable), (0, 1));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_pick_not_read_in_full_is_kept() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = std::env::temp_dir().join(format!("rustgis-copies-locked-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let picked = root.join("picked");
        let locked = picked.join("1/0");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::write(locked.join("a.gpkg"), b"a").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Skipped where permissions don't hold (running as root).
        if std::fs::read_dir(&locked).is_err() {
            let found = unused(&picked, &of(json!({})), &AtomicBool::new(false)).unwrap();
            assert_eq!((found.picks.len(), found.unreadable), (0, 1));
        }
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn scanning_stops_while_the_project_is_being_written_out() {
        let root = std::env::temp_dir().join(format!("rustgis-copies-stop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let picked = root.join("picked");
        std::fs::create_dir_all(picked.join("1/0")).unwrap();
        std::fs::write(picked.join("1/0/a.gpkg"), b"a").unwrap();
        let cancelled = AtomicBool::new(false);
        let written = std::cell::Cell::new(0usize);
        // A project of two million strings, and Cancel pressed a thousand strings in.
        let endless = |out: &mut dyn Write| -> Result<(), serde_json::Error> {
            for n in 0..2_000_000usize {
                serde_json::to_writer(&mut *out, "abcdefgh")?;
                written.set(n + 1);
                if n == 1_000 {
                    cancelled.store(true, Ordering::Relaxed);
                }
            }
            Ok(())
        };
        let stopped = unused(&picked, &endless, &cancelled);
        assert_eq!(stopped, Err("Stopped.".to_string()));
        // A few buffers past the press, not the whole run.
        assert!(written.get() < 100_000, "{} strings written after Cancel", written.get());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_scan_holds_no_more_than_a_buffer_of_the_project() {
        let prefix = b"/data/picked".to_vec();
        let (small, long) = (b"/1/0/a.shp".to_vec(), b"/22/0/a-much-longer-name.gpkg".to_vec());
        let cancelled = AtomicBool::new(false);
        let mut names = Names::new(prefix.clone(), HashSet::from([small.clone(), long.clone()]), &cancelled);
        let bound = prefix.len() + names.reach;
        let junk = b"{\"a\":\"/data/pic\",\"b\":\"/data/picked/9/9/none\"},".repeat(1500);
        assert!(junk.len() > WRITE);
        let mut total = 0;
        while total < 16 * 1024 * 1024 {
            names.write_all(&junk).unwrap();
            total += junk.len();
            assert!(names.carry.len() <= bound, "{} bytes held of {total}", names.carry.len());
            if total > 8 * 1024 * 1024 && names.found.is_empty() {
                // One path among them, cut in two writes.
                names.write_all(b"\"/data/picked/22/0/a-much-").unwrap();
                names.write_all(b"longer-name.gpkg\"").unwrap();
            }
        }
        names.look(true);
        assert_eq!(names.found, HashSet::from([long]));
    }

    #[test]
    fn one_huge_write_is_taken_a_buffer_at_a_time() {
        let prefix = b"/data/picked".to_vec();
        let (first, last) = (b"/1/0/a.shp".to_vec(), b"/22/0/b.gpkg".to_vec());
        let cancelled = AtomicBool::new(false);
        let mut names = Names::new(prefix.clone(), HashSet::from([first.clone(), last.clone()]), &cancelled);
        let bound = prefix.len() + names.reach;
        // A thumbnail or a long text is written whole, in one call. A path in it is cut where the
        // scan takes it in pieces, and another is at its end.
        let mut big = vec![b'x'; 8 * 1024 * 1024];
        for (path, at) in [(b"/data/picked/1/0/a.shp".as_slice(), WRITE - 5), (b"/data/picked/22/0/b.gpkg".as_slice(), big.len() - 24)] {
            big[at..at + path.len()].copy_from_slice(path);
        }
        names.write_all(&big).unwrap();
        names.look(true);
        assert_eq!(names.found, HashSet::from([first, last]));
        assert!(names.carry.capacity() <= 2 * WRITE + bound, "{} bytes reserved for one write", names.carry.capacity());
    }

    #[test]
    fn the_writing_ends_once_every_path_is_found() {
        let root = std::env::temp_dir().join(format!("rustgis-copies-found-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let picked = root.join("picked");
        let file = picked.join("1").join("0").join("a.gpkg");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, b"a").unwrap();
        let written = std::cell::Cell::new(0usize);
        // The path first, then two million strings: all but a few buffers are never written.
        let project = |out: &mut dyn Write| -> Result<(), serde_json::Error> {
            serde_json::to_writer(&mut *out, &file.to_string_lossy())?;
            for n in 0..2_000_000usize {
                serde_json::to_writer(&mut *out, "abcdefgh")?;
                written.set(n + 1);
            }
            Ok(())
        };
        let found = unused(&picked, &project, &AtomicBool::new(false)).unwrap();
        assert!(found.picks.is_empty());
        assert!(written.get() < 100_000, "{} strings written after the path", written.get());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn cancel_pressed_in_the_last_write_still_stops() {
        let root = std::env::temp_dir().join(format!("rustgis-copies-last-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let picked = root.join("picked");
        std::fs::create_dir_all(picked.join("1/0")).unwrap();
        std::fs::write(picked.join("1/0/a.gpkg"), b"a").unwrap();
        let cancelled = AtomicBool::new(false);
        let project = |out: &mut dyn Write| -> Result<(), serde_json::Error> {
            serde_json::to_writer(&mut *out, "short")?;
            cancelled.store(true, Ordering::Relaxed);
            Ok(())
        };
        assert_eq!(unused(&picked, &project, &cancelled), Err("Stopped.".to_string()));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_scan_answers_as_the_text_search_did() {
        let (unix, windows) = ("/data/picked", r"C:\data\picked");
        let a = "/data/picked/1/0/a.shp";
        let in_json = |file: &str| json!({"path": file}).to_string();
        let cases: Vec<(&str, String, &str, bool)> = vec![
            (unix, in_json(a), a, true),
            (unix, in_json(a), "/data/picked/1/0/b.shp", false),
            // Inside a longer string, before and after.
            (unix, in_json("/vsizip//data/picked/1/0/a.zip/x.shp"), "/data/picked/1/0/a.zip", true),
            (unix, in_json("file:///data/picked/1/0/a.shp?x=1"), a, true),
            (unix, in_json("/data/picked/1/0/a.gpkg|layername=roads"), "/data/picked/1/0/a.gpkg", true),
            // At the very start and the very end of the text.
            (unix, a.to_string(), a, true),
            (unix, format!("xx{a}"), a, true),
            // Two back to back.
            (unix, format!("{a}/data/picked/1/0/b.shp"), "/data/picked/1/0/b.shp", true),
            (unix, format!("{a}/data/picked/1/0/b.shp"), "/data/picked/1/0/c.shp", false),
            // The folder inside a path.
            (unix, "/data/picked/data/picked/1/0/a.shp".to_string(), a, true),
            // A folder beside it, and text cut short.
            (unix, in_json("/data/picked2/1/0/a.shp"), a, false),
            (unix, "/data/picked/1/0/a.sh".to_string(), a, false),
            (unix, "/data/pick".to_string(), a, false),
            // Names JSON writes differently.
            (unix, in_json("/data/picked/1/0/a \"q\" b.shp"), "/data/picked/1/0/a \"q\" b.shp", true),
            (unix, in_json("/data/picked/1/0/a \"q\" b.shp"), "/data/picked/1/0/a 'q' b.shp", false),
            (unix, in_json("/data/picked/1/0/a\\b\nc.shp"), "/data/picked/1/0/a\\b\nc.shp", true),
            (unix, in_json("/data/picked/1/0/é日本.shp"), "/data/picked/1/0/é日本.shp", true),
            // The Windows form: backslashes, doubled in JSON.
            (windows, in_json(r"C:\data\picked\1\0\a.shp"), r"C:\data\picked\1\0\a.shp", true),
            (windows, in_json(r"C:\data\picked\1\0\a.shp"), r"C:\data\picked\1\0\b.shp", false),
        ];
        for (picked, text, file, expected) in &cases {
            let escaped_file = escaped(file);
            assert_eq!(text.contains(&escaped_file), *expected, "the text search on {text} for {file}");
            let prefix = escaped(picked).into_bytes();
            let (file, decoy, longer) = (
                PathBuf::from(file),
                PathBuf::from(picked).join("1").join("0").join("decoy.shp"),
                PathBuf::from(picked).join("1").join("0").join("a-longer-decoy-than-the-others.shp"),
            );
            let rests: HashSet<Vec<u8>> = [&file, &decoy, &longer].into_iter().filter_map(|f| rest_of(&prefix, f)).collect();
            // Whole, and cut in every way that crosses the folder or a rest.
            for step in [1, 3, 7, 4096, usize::MAX] {
                let cancelled = AtomicBool::new(false);
                let mut names = Names::new(prefix.clone(), rests.clone(), &cancelled);
                for part in text.as_bytes().chunks(step.min(text.len()).max(1)) {
                    names.write_all(part).unwrap();
                }
                names.look(true);
                let named = rest_of(&prefix, &file).is_some_and(|rest| names.found.contains(&rest));
                assert_eq!(named, *expected, "the scan of {text} for {} in writes of {step}", file.display());
            }
            // And in two writes, cut at every place.
            for at in 0..=text.len() {
                let cancelled = AtomicBool::new(false);
                let mut names = Names::new(prefix.clone(), rests.clone(), &cancelled);
                let (before, after) = text.as_bytes().split_at(at);
                names.write_all(before).unwrap();
                names.write_all(after).unwrap();
                names.look(true);
                let named = rest_of(&prefix, &file).is_some_and(|rest| names.found.contains(&rest));
                assert_eq!(named, *expected, "the scan of {text} for {} in two writes cut at {at}", file.display());
            }
        }
    }

    #[test]
    fn a_real_project_is_scanned_for_every_place_a_path_can_be() {
        use rustgis_ui_egui::rustgis_engine::Project;
        use rustgis_ui_egui::rustgis_engine::data::{Dataset, Field, FieldType, GeometryType, Value};
        use rustgis_ui_egui::rustgis_engine::map::Layer;
        let root = std::env::temp_dir().join(format!("rustgis-copies-real-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let picked = root.join("picked");
        let file = |pick: &str| picked.join(pick).join("0").join("f.gpkg");
        for pick in ["1", "2", "3", "4", "5", "6"] {
            std::fs::create_dir_all(file(pick).parent().unwrap()).unwrap();
            std::fs::write(file(pick), b"f").unwrap();
        }
        // Paths a walk of the layers' sources would not meet: in a notebook's text, in what a
        // tool ran with, in a folder connection.
        let mut value = serde_json::to_value(Project::default()).unwrap();
        value["notebooks"] =
            json!([{"name": "n", "cells": [{"kind": "code", "source": format!("layer.add path={}", file("3").display())}]}]);
        value["gp_history"] = json!([{"tool": "Buffer", "ok": true, "messages": [], "millis": 0, "params": {"input": file("4")}}]);
        value["folders"] = json!([file("5")]);
        let mut project: Project = serde_json::from_value(value).unwrap();
        // A layer's source, and a photo in a row's attributes.
        let mut sourced = Dataset::new("Roads", GeometryType::Point, vec![]);
        sourced.source = Some(file("1").to_string_lossy().into_owned());
        let mut photos = Dataset::new("Trees", GeometryType::Point, vec![Field::new("photo", FieldType::Text)]);
        photos.push(None, vec![Value::Text(file("2").to_string_lossy().into_owned())]);
        let map = project.maps.first_mut().unwrap();
        map.layers.push(Layer::new(1, sourced));
        map.layers.push(Layer::new(2, photos));
        let found = unused(&picked, &|out| serde_json::to_writer(out, &project), &AtomicBool::new(false)).unwrap();
        assert_eq!(found.picks, [picked.join("6")]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn picks_with_nothing_to_judge_leave_the_project_unwritten() {
        let root = std::env::temp_dir().join(format!("rustgis-copies-none-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let picked = root.join("picked");
        std::fs::create_dir_all(picked.join("3/0")).unwrap();
        let wrote = std::cell::Cell::new(false);
        let found = unused(
            &picked,
            &|out| {
                wrote.set(true);
                serde_json::to_writer(out, "x")
            },
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(!wrote.get() && found.picks.is_empty() && found.busy_picks == [picked.join("3")]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn sizes_are_said_in_megabytes_or_kilobytes() {
        assert_eq!(size(420), "1 KB");
        assert_eq!(size(50 * 1024), "50 KB");
        assert_eq!(size(3 * 1024 * 1024 + 300 * 1024), "3.3 MB");
    }
}
