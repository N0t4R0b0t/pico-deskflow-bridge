//! The server's clipboard, typed out on the laptop on request (the paste
//! hotkey or the web page). The parsing and keystroke generation are in
//! `deskflow_proto::{clipboard, typing}`.
//!
//! The text lives in one buffer ([`MAX_TEXT`] bytes): the Deskflow task
//! appends to it as `DCLP` data arrives, and the typing task reads it one
//! character at a time, so neither holds the lock across an await. A new
//! clipboard bumps a generation number, which stops typing the old one.

use core::cell::RefCell;

use deskflow_proto::clipboard::utf8_prefix;
use deskflow_proto::settings::Settings;
use deskflow_proto::typing::{self, Keystroke, RELEASE};
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};
use heapless::Vec;
use portable_atomic::{AtomicBool, AtomicU32, Ordering};

use crate::{Outgoing, REPORTS, board, status};

/// Bytes of clipboard text kept; the rest of a larger clipboard is dropped.
pub const MAX_TEXT: usize = board::CLIPBOARD_MAX;
/// How long to wait for the hotkey's modifiers to be released before
/// typing anyway (they'd otherwise combine with the typed keys).
const RELEASE_WAIT: Duration = Duration::from_secs(3);

struct Clipboard {
    bytes: Vec<u8, MAX_TEXT>,
    /// More text was sent than fits.
    truncated: bool,
    /// The transfer has finished (the text is complete and valid UTF-8).
    complete: bool,
    /// Bumped by every new clipboard.
    generation: u32,
}

static CLIPBOARD: Mutex<CriticalSectionRawMutex, RefCell<Clipboard>> =
    Mutex::new(RefCell::new(Clipboard { bytes: Vec::new(), truncated: false, complete: false, generation: 0 }));
/// Characters in the complete text, for the page.
static CHARS: AtomicU32 = AtomicU32::new(0);
static TRUNCATED: AtomicBool = AtomicBool::new(false);

static REQUEST: Signal<CriticalSectionRawMutex, ()> = Signal::new();
static ABORT: AtomicBool = AtomicBool::new(false);
static TYPING: AtomicBool = AtomicBool::new(false);
static TYPED: AtomicU32 = AtomicU32::new(0);
/// Set by the Deskflow task: whether the user is holding any key.
static KEYS_HELD: AtomicBool = AtomicBool::new(false);

/// A new clipboard is arriving: drops the old text (and stops typing it).
pub fn begin() {
    CLIPBOARD.lock(|c| {
        let mut c = c.borrow_mut();
        c.bytes.clear();
        c.truncated = false;
        c.complete = false;
        c.generation = c.generation.wrapping_add(1);
    });
    CHARS.store(0, Ordering::Relaxed);
    TRUNCATED.store(false, Ordering::Relaxed);
}

/// More text of the arriving clipboard.
pub fn append(text: &[u8]) {
    CLIPBOARD.lock(|c| {
        let mut c = c.borrow_mut();
        let room = MAX_TEXT - c.bytes.len();
        let _ = c.bytes.extend_from_slice(&text[..text.len().min(room)]);
        c.truncated |= text.len() > room;
    });
}

/// The arriving clipboard is complete.
pub fn finish() {
    let (chars, truncated) = CLIPBOARD.lock(|c| {
        let mut c = c.borrow_mut();
        // The size limit can cut a character in half.
        let valid = utf8_prefix(&c.bytes);
        c.truncated |= valid < c.bytes.len();
        c.bytes.truncate(valid);
        c.complete = true;
        let text = core::str::from_utf8(&c.bytes).unwrap_or("");
        (text.chars().count(), c.truncated)
    });
    CHARS.store(chars as u32, Ordering::Relaxed);
    TRUNCATED.store(truncated, Ordering::Relaxed);
    if truncated {
        log::info!("clipboard: kept the first {} characters of the server's clipboard", chars);
    } else {
        log::info!("clipboard: {} characters from the server", chars);
    }
}

/// Starts typing the clipboard (unless already typing).
pub fn request_typing() {
    if !TYPING.load(Ordering::Relaxed) {
        REQUEST.signal(());
    }
}

/// Stops typing after the current key.
pub fn stop_typing() {
    ABORT.store(true, Ordering::Relaxed);
}

pub fn is_typing() -> bool {
    TYPING.load(Ordering::Relaxed)
}

pub fn set_keys_held(held: bool) {
    KEYS_HELD.store(held, Ordering::Relaxed);
}

/// (characters held, truncated, characters typed so far if typing)
pub fn status() -> (usize, bool, Option<usize>) {
    let typing = is_typing().then(|| TYPED.load(Ordering::Relaxed) as usize);
    (CHARS.load(Ordering::Relaxed) as usize, TRUNCATED.load(Ordering::Relaxed), typing)
}

/// The character at `pos` of the text of `generation`, or `None` at the end
/// or once a new clipboard replaced it.
fn char_at(generation: u32, pos: usize) -> Option<(char, usize)> {
    CLIPBOARD.lock(|c| {
        let c = c.borrow();
        if c.generation != generation {
            return None;
        }
        typing::char_at(&c.bytes, pos)
    })
}

#[embassy_executor::task]
pub async fn typing_task(settings: &'static Settings) -> ! {
    // Two reports per character.
    let interval = Duration::from_micros(500_000 / settings.typing_rate.max(1) as u64);
    loop {
        REQUEST.wait().await;
        if !status::ble_connected() {
            log::warn!("clipboard: the laptop isn't connected; not typing");
            continue;
        }
        let (generation, ready, empty) =
            CLIPBOARD.lock(|c| {
                let c = c.borrow();
                (c.generation, c.complete, c.bytes.is_empty())
            });
        if !ready {
            log::info!("clipboard: still arriving from the server; try again in a moment");
            continue;
        }
        if empty {
            log::info!("clipboard: nothing to type (copy some text on the server first)");
            continue;
        }

        let deadline = Instant::now() + RELEASE_WAIT;
        while KEYS_HELD.load(Ordering::Relaxed) && Instant::now() < deadline {
            Timer::after_millis(20).await;
        }

        ABORT.store(false, Ordering::Relaxed);
        TYPED.store(0, Ordering::Relaxed);
        TYPING.store(true, Ordering::Relaxed);
        log::info!("clipboard: typing {} characters", CHARS.load(Ordering::Relaxed));
        let (mut pos, mut typed, mut skipped, mut stopped) = (0, 0u32, 0u32, false);
        while let Some((c, next)) = char_at(generation, pos) {
            pos = next;
            if ABORT.load(Ordering::Relaxed) {
                stopped = true;
                break;
            }
            match typing::keystroke(settings.typing_layout, c) {
                Keystroke::Press(report) => {
                    REPORTS.send(Outgoing::typed(report)).await;
                    Timer::after(interval).await;
                    REPORTS.send(Outgoing::typed(RELEASE)).await;
                    Timer::after(interval).await;
                    status::note_activity();
                    typed += 1;
                    TYPED.store(typed, Ordering::Relaxed);
                }
                Keystroke::Ignore => {}
                Keystroke::Unsupported => skipped += 1,
            }
        }
        // A new clipboard arriving ends the loop early too.
        stopped |= char_at(generation, 0).is_none();
        // Never leave a typed key held, even if the queue dropped a release.
        REPORTS.send(Outgoing::typed(RELEASE)).await;
        TYPING.store(false, Ordering::Relaxed);
        let layout = settings.typing_layout.name();
        match (stopped, skipped) {
            (true, _) => log::info!("clipboard: stopped after {} characters", typed),
            (false, 0) => log::info!("clipboard: typed {} characters", typed),
            (false, n) => log::info!("clipboard: typed {} characters, skipped {} the '{}' layout can't type", typed, n, layout),
        }
    }
}
