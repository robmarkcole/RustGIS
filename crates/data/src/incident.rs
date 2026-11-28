//! A bug in RustGIS met while a command or a tool ran, caught before it could take the app down:
//! a short reference that ties the message the user sees to the details kept for a bug report
//! (the panic's message and where in the code it happened) and to the app's crash log.
//!
//! [`install`] records each panic as it happens, on whatever thread, then hands it on to the hook
//! that was there before (the desktop app's crash log). A catch site turns what it caught into an
//! [`Incident`] with [`caught`], and says [`message`]; [`find`] gives the details back by reference.

use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard, Once};

/// The incidents kept for their details: a session meets few, and a long one keeps the newest.
const KEPT: usize = 16;

/// A bug met and caught: its reference, the panic's message and where it happened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Incident {
    /// Six hexadecimal digits, as the user sees them and the crash log names them.
    pub id: String,
    pub message: String,
    /// `file:line` in RustGIS's source, when the panic gave it.
    pub location: Option<String>,
}

impl Incident {
    /// The details a bug report needs, as Messages shows them under the failure.
    pub fn details(&self) -> String {
        let at = self.location.as_deref().map(|at| format!("\nat {at}")).unwrap_or_default();
        format!("Incident {} in RustGIS {}\n{}{at}", self.id, env!("CARGO_PKG_VERSION"), self.message)
    }
}

static RECENT: Mutex<VecDeque<Incident>> = Mutex::new(VecDeque::new());
static COUNT: Mutex<u64> = Mutex::new(0);
static INSTALLED: Once = Once::new();

std::thread_local! {
    /// The reference of the last panic on this thread, for the catch site and the crash log.
    static LAST: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A reference no incident kept has: the time and a count, mixed, as six hexadecimal digits.
fn new_id() -> String {
    loop {
        let id = mixed_id();
        if find(&id).is_none() {
            return id;
        }
    }
}

/// A reference from the time and a count, mixed, as six hexadecimal digits.
fn mixed_id() -> String {
    let n = {
        let mut count = lock(&COUNT);
        *count = count.wrapping_add(1);
        *count
    };
    let mixed = crate::diagnostics::now_ms().wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(n.wrapping_mul(0xBF58_476D_1CE4_E5B9));
    format!("{:06X}", (mixed >> 40) & 0xFF_FFFF)
}

/// Keeps `incident` for its details, the oldest let go past [`KEPT`].
fn keep(incident: Incident) {
    let mut recent = lock(&RECENT);
    if recent.len() >= KEPT {
        recent.pop_front();
    }
    recent.push_back(incident);
}

/// A panic's message, whole: matched whole to tell one panic from another, and scrubbed whole
/// (a login cut in two isn't recognised) before Messages bounds it for showing. A session keeps
/// [`KEPT`] at most.
fn payload_text(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a panic without a message".to_string())
}

/// Records every panic as an incident before the hook that was there runs (the app's crash log,
/// which can name it by [`last_id`]). Installing it again does nothing.
pub fn install() {
    INSTALLED.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let id = new_id();
            let message = payload_text(info.payload());
            let location = info.location().map(|l| format!("{}:{}", l.file(), l.line()));
            LAST.with(|last| *last.borrow_mut() = Some(id.clone()));
            keep(Incident { id, message, location });
            previous(info);
        }));
    });
}

/// The reference of the last panic on this thread, if [`install`] recorded one.
pub fn last_id() -> Option<String> {
    LAST.with(|last| last.borrow().clone())
}

/// The incident a catch site caught: the one [`install`] recorded for this panic, or, without the
/// hook, one made from what was caught. The panic is told by its message: this thread's last one
/// when it matches (a catch site that drops what it caught leaves an older one there), else the
/// newest kept with that message, as a worker's panic raised again on its caller's thread is
/// (`analysis::parallel`), so it keeps the reference the crash log gave it.
pub fn caught(payload: &(dyn std::any::Any + Send)) -> Incident {
    let message = payload_text(payload);
    let last = LAST.with(|last| last.borrow_mut().take());
    let found = last
        .and_then(|id| find(&id))
        .filter(|found| found.message == message)
        .or_else(|| lock(&RECENT).iter().rev().find(|i| i.message == message).cloned());
    if let Some(found) = found {
        return found;
    }
    let incident = Incident { id: new_id(), message, location: None };
    keep(incident.clone());
    incident
}

/// The incidents kept, oldest first: for a bug report.
pub fn kept() -> Vec<Incident> {
    lock(&RECENT).iter().cloned().collect()
}

/// An incident kept by its reference.
pub fn find(id: &str) -> Option<Incident> {
    lock(&RECENT).iter().rev().find(|i| i.id == id).cloned()
}

/// What a caught bug says: what failed, that it is a bug in RustGIS, its reference, and, when the
/// project was put back as it was (`restored`), that nothing it did was kept. It keeps the words
/// "failed unexpectedly", which tests and scripts look for to tell a bug from a refusal.
pub fn message(what: &str, restored: bool, id: &str) -> String {
    let kept = if restored { "; the project is as it was before" } else { "" };
    format!(
        "{what} failed unexpectedly, a bug in RustGIS (incident {id}){kept}. The details are in Messages; please include them in a bug report"
    )
}

/// What a bug caught while doing `what` says ([`message`]), its incident taken from the panic
/// `payload` ([`caught`]): for a catch site that rolls nothing back.
pub fn bug(what: &str, payload: &(dyn std::any::Any + Send)) -> String {
    let incident = caught(payload);
    message(what, false, &incident.id)
}

/// The kept incident `text` names by its reference (`… (incident 7F3A2C)…`, in any language the
/// message is said in), the newest if several.
pub fn named(text: &str) -> Option<Incident> {
    lock(&RECENT).iter().rev().find(|i| text.contains(&format!("{})", i.id))).cloned()
}

/// The reference a message from [`message`] names, if it names one.
pub fn named_in(text: &str) -> Option<&str> {
    let (_, rest) = text.split_once("(incident ")?;
    let id = rest.get(..6)?;
    id.bytes().all(|b| b.is_ascii_hexdigit()).then_some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find_by_message(message: &str) -> Option<Incident> {
        lock(&RECENT).iter().rev().find(|i| i.message == message).cloned()
    }

    #[test]
    fn a_caught_panic_is_an_incident_with_its_place_and_a_message_naming_it() {
        install();
        let payload = std::panic::catch_unwind(|| panic!("the bug")).unwrap_err();
        let incident = caught(payload.as_ref());
        assert_eq!(incident.message, "the bug");
        assert!(incident.location.as_deref().is_some_and(|at| at.contains("incident.rs")), "{incident:?}");
        assert_eq!(find(&incident.id).as_ref(), Some(&incident));
        let said = message("Buffer", true, &incident.id);
        assert!(said.contains("failed unexpectedly"), "the words tests look for: {said}");
        assert!(said.starts_with("Buffer failed unexpectedly, a bug in RustGIS (incident "), "{said}");
        assert!(said.contains("the project is as it was before"), "{said}");
        assert_eq!(named_in(&said), Some(incident.id.as_str()));
        assert!(incident.details().contains("the bug") && incident.details().contains(&incident.id));
        // Caught once: the next catch on this thread doesn't take it again.
        assert_eq!(last_id(), None);
        // Raised on another thread and caught here (a worker's, raised again on its caller), it
        // keeps its reference.
        let worker = std::thread::scope(|s| {
            std::thread::Builder::new()
                .spawn_scoped(s, || std::panic::catch_unwind(|| panic!("the worker's bug")).unwrap_err())
                .unwrap()
                .join()
        })
        .unwrap();
        let on_worker = find_by_message("the worker's bug");
        assert_eq!(Some(caught(worker.as_ref()).id), on_worker.map(|i| i.id));
        // A stale reference from an earlier panic, its payload dropped, isn't given to another.
        let _ = std::panic::catch_unwind(|| panic!("dropped"));
        let other = caught(&"a different payload");
        assert_eq!(other.message, "a different payload");
        assert_eq!(other.location, None);
        assert_eq!(named_in("Buffer failed: no layer"), None);
    }
}
