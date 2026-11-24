//! The loop, driven through a real pty.
//!
//! ─── Why this exists ─────────────────────────────────────────────────────
//!
//! Every unit test in this crate is of a helper. The thing that was actually
//! wrong, each time it was wrong, was `app::event_loop` -- a list left stale
//! beside a sidebar, a key acting on a row that was not on screen, a ctrl-c
//! that took the whole program with it -- and no unit test reaches it, because
//! it needs a terminal to run at all.
//!
//! So this gives it one. Each check starts a small screen on a pty, types at
//! it, and reads what a terminal would be showing.
//!
//! ─── Why it is one binary with no test harness ───────────────────────────
//!
//! The screens under test have to run in a CHILD, on the pty. Rather than
//! locate a second binary, this one runs itself: with `SCREEN` set it is the
//! child and shows that screen; without, it is the driver. That needs its own
//! `main`, hence `harness = false` in Cargo.toml.
//!
//! ─── Why nothing here sleeps and hopes ───────────────────────────────────
//!
//! A check that types a key and then waits 300ms is a check that fails on a
//! slow runner. Every expectation here is a `wait` for the screen to show
//! something, with a long deadline that only a failure ever reaches.
//!
//! A NEGATIVE one -- "this key must not have opened a field" -- is done with a
//! second key whose effect depends on the first. Keys are read in order, so
//! once the second one's effect is on screen the first has been handled, and
//! what it did or did not do is there to read.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use tui_core::{Action, Flow, Host, Pane, Row, Screen, Split};

/// Set in the child: which screen to show.
const SCREEN: &str = "TUI_CORE_PTY_SCREEN";
/// Only a failing check waits this long.
const DEADLINE: Duration = Duration::from_secs(10);

fn main() {
    if let Ok(which) = std::env::var(SCREEN) {
        return show(&which);
    }
    // `cargo test -- name` narrows, as it does for the other tests. Flags
    // meant for the standard harness are ignored rather than tripped over.
    let wanted: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| !a.starts_with('-'))
        .collect();
    let mut failed = 0;
    for (name, check) in CHECKS {
        if !wanted.is_empty() && !wanted.iter().any(|w| name.contains(w.as_str())) {
            continue;
        }
        match check() {
            Ok(()) => println!("test {name} ... ok"),
            Err(why) => {
                failed += 1;
                println!("test {name} ... FAILED\n{why}\n");
            }
        }
    }
    if failed > 0 {
        println!("{failed} pty check(s) failed");
        std::process::exit(1);
    }
}

type Check = fn() -> Result<(), String>;

const CHECKS: &[(&str, Check)] = &[
    (
        "a_sidebar_pick_reaches_the_list",
        a_sidebar_pick_reaches_the_list,
    ),
    (
        "the_sidebar_wears_its_own_brackets",
        the_sidebar_wears_its_own_brackets,
    ),
    (
        "a_sidebar_screen_has_no_pane_to_enter",
        a_sidebar_screen_has_no_pane_to_enter,
    ),
    (
        "a_question_can_be_answered_with_a_question",
        a_question_can_be_answered_with_a_question,
    ),
    (
        "a_filter_stays_on_its_own_tab",
        a_filter_stays_on_its_own_tab,
    ),
    (
        "clearing_a_filter_keeps_the_cursor_on_its_row",
        clearing_a_filter_keeps_the_cursor_on_its_row,
    ),
    (
        "a_filter_hiding_every_row_sends_no_keys",
        a_filter_hiding_every_row_sends_no_keys,
    ),
    (
        "a_scrolled_pane_does_not_follow_the_cursor",
        a_scrolled_pane_does_not_follow_the_cursor,
    ),
    (
        "a_long_label_still_wears_the_brackets",
        a_long_label_still_wears_the_brackets,
    ),
    ("a_resize_redraws_the_frame", a_resize_redraws_the_frame),
    (
        "ctrl_c_stops_the_child_and_not_the_tui",
        ctrl_c_stops_the_child_and_not_the_tui,
    ),
    (
        "the_mouse_is_left_alone_and_pastes_are_bracketed",
        the_mouse_is_left_alone_and_pastes_are_bracketed,
    ),
    (
        "a_paste_is_typed_into_a_field_and_dropped_outside_one",
        a_paste_is_typed_into_a_field_and_dropped_outside_one,
    ),
    (
        "a_late_probe_reply_is_not_typed",
        a_late_probe_reply_is_not_typed,
    ),
    (
        "one_keypress_shows_the_brackets_opening",
        one_keypress_shows_the_brackets_opening,
    ),
    (
        "an_idle_screen_writes_nothing",
        an_idle_screen_writes_nothing,
    ),
];

// ─── The checks ──────────────────────────────────────────────────────────

/// The one that mattered. A sidebar pick changes what the list holds and
/// returns `Flow::Dirty`; the list was never re-read, so it went on showing
/// the old rows while `enter` opened the new row at that position.
fn a_sidebar_pick_reaches_the_list() -> Result<(), String> {
    let mut t = Term::open("side", 24, 100)?;
    t.wait("the first frame", |t| t.shows("alpha-0"))?;
    t.send("h")?; // list -> sidebar
    t.send("j")?; // onto pick-bravo
    t.send("\r")?;
    t.wait("the list to follow the pick", |t| {
        t.shows("bravo-0") && !t.shows("alpha-0")
    })?;
    t.send("l")?; // back to the list
    t.send("j")?; // the second row DRAWN
    t.send("\r")?;
    t.wait("enter to open the row that is drawn", |t| {
        t.shows("opened bravo-1")
    })
}

/// The sidebar shared the list's brackets, so they sat on whichever sidebar
/// row had the LIST cursor's number.
fn the_sidebar_wears_its_own_brackets() -> Result<(), String> {
    let mut t = Term::open("side", 24, 100)?;
    t.wait("the first frame", |t| t.shows("alpha-0"))?;
    t.send("h")?;
    t.send("j")?;
    t.wait("the brackets on the sidebar's cursor", |t| {
        t.bracketed().as_deref() == Some("pick-bravo")
    })
}

/// `l` on a sidebar screen's list used to move focus into a pane the screen
/// does not have. If it still did, the `h` after it would only bring focus
/// BACK to the list; with nothing entered, `h` crosses to the sidebar.
fn a_sidebar_screen_has_no_pane_to_enter() -> Result<(), String> {
    let mut t = Term::open("side", 24, 100)?;
    t.wait("the first frame", |t| t.shows("alpha-0"))?;
    t.send("l")?;
    t.send("h")?;
    t.wait("h to cross to the sidebar", |t| {
        t.footer().contains("enter pick")
    })
}

/// `answered` returning `Flow::Ask` got no second field.
fn a_question_can_be_answered_with_a_question() -> Result<(), String> {
    let mut t = Term::open("plain", 24, 100)?;
    t.wait("the first frame", |t| t.shows("plain-0"))?;
    t.send("a")?;
    t.wait("the first question", |t| t.footer().contains("first:"))?;
    t.send("one\r")?;
    t.wait("the second question", |t| t.footer().contains("second:"))?;
    t.send("two\r")?;
    t.wait("both answers to arrive", |t| {
        t.footer().contains("answers=one,two")
    })
}

/// A filter typed on one tab emptied the next one.
fn a_filter_stays_on_its_own_tab() -> Result<(), String> {
    let mut t = Term::open("tabs", 24, 100)?;
    t.wait("the first frame", |t| t.shows("one-0"))?;
    t.send("/")?;
    t.send("one-1\r")?;
    t.wait("the filter to be kept", |t| {
        t.footer().contains("filtering on \"one-1\"")
    })?;
    t.send("k")?; // already on the first row: the door to the strip
    t.wait("focus on the strip", |t| t.footer().contains("pick a tab"))?;
    t.send("l")?;
    t.wait("the other tab's rows", |t| t.shows("two-0"))
}

/// `esc` on a kept filter reused the cursor's POSITION as a row index, so it
/// landed on an unrelated row.
fn clearing_a_filter_keeps_the_cursor_on_its_row() -> Result<(), String> {
    let mut t = Term::open("plain", 40, 100)?;
    t.wait("the first frame", |t| t.shows("plain-0"))?;
    t.send("/")?;
    t.send("plain-2\r")?;
    t.send("j")?;
    t.send("j")?;
    t.wait("the cursor on the third match", |t| {
        t.bracketed().as_deref() == Some("plain-21")
    })?;
    t.send("\x1b")?;
    t.wait("everything back, cursor where it was", |t| {
        t.shows("plain-0 ") && t.bracketed().as_deref() == Some("plain-21")
    })
}

/// With every row filtered away the cursor's row fell back to 0, and a key
/// acted on it unseen. Here `a` would open a field; the `/` after it would
/// then be typed INTO that field and no filter field would ever appear.
fn a_filter_hiding_every_row_sends_no_keys() -> Result<(), String> {
    let mut t = Term::open("plain", 24, 100)?;
    t.wait("the first frame", |t| t.shows("plain-0"))?;
    t.send("/")?;
    t.send("zzzz\r")?;
    t.wait("the filter to be kept", |t| {
        t.footer().contains("filtering on \"zzzz\"")
    })?;
    t.send("a")?;
    t.send("/")?;
    t.wait("the filter field, and no question", |t| {
        t.footer().contains("filter: zzzz")
    })
}

/// A scroll offset belongs to the row it was scrolled on. It used to carry
/// over, so the next row's pane opened part-way down.
fn a_scrolled_pane_does_not_follow_the_cursor() -> Result<(), String> {
    let mut t = Term::open("plain", 24, 100)?;
    t.wait("the first frame", |t| t.shows("pane 0 line 0"))?;
    t.send("l")?;
    t.send("j")?;
    t.send("j")?;
    t.wait("the pane to scroll", |t| {
        t.line(1).contains("pane 0 line 2")
    })?;
    t.send("h")?;
    t.send("j")?;
    t.wait("the next row's pane from its top", |t| {
        t.line(1).contains("pane 1 line 0")
    })
}

/// A label truncated to the column left no cell for the closing bracket, so
/// such a row never wore them.
fn a_long_label_still_wears_the_brackets() -> Result<(), String> {
    let mut t = Term::open("long", 24, 100)?;
    t.wait("brackets inside the left column", |t| {
        let column: String = t.grid.cells[1].iter().take(52).collect();
        column.contains('[') && column.contains(']') && column.contains('\u{2026}')
    })
}

/// A resize redrew the rows and nothing else: the divider stopped where the
/// old screen ended.
fn a_resize_redraws_the_frame() -> Result<(), String> {
    let mut t = Term::open("plain", 24, 100)?;
    t.wait("the first frame", |t| t.shows("plain-0"))?;
    t.resize(34, 100)?;
    t.wait("the divider on the new rows", |t| {
        t.line(28).contains('\u{2502}')
    })
}

/// ctrl-c while a detached child has the terminal signals the whole
/// foreground group, this program included, and its default answer was to die.
fn ctrl_c_stops_the_child_and_not_the_tui() -> Result<(), String> {
    let mut t = Term::open("plain", 24, 100)?;
    t.wait("the first frame", |t| t.shows("plain-0"))?;
    t.send("d")?;
    // Not before the child says it is running: until then the tty may still
    // be raw, and the byte would be a keystroke rather than a signal.
    t.wait("the child to start", |t| t.wrote(b"ready"))?;
    t.send("\x03")?;
    t.wait("the child to be interrupted", |t| t.wrote(b"interrupted"))?;
    if !t.alive() {
        return Err("  the TUI died with its child".into());
    }
    t.grid.clear();
    t.send("\r")?;
    t.wait("the screen to come back", |t| t.shows("plain-0"))
}

/// Nothing reads the mouse, so it is not asked for. And because it is not, a
/// middle click pastes -- so pastes are asked to arrive bracketed.
fn the_mouse_is_left_alone_and_pastes_are_bracketed() -> Result<(), String> {
    let mut t = Term::open("plain", 24, 100)?;
    t.wait("the first frame", |t| t.shows("plain-0"))?;
    if t.wrote(b"\x1b[?1000h") || t.wrote(b"\x1b[?1003h") {
        return Err("  mouse reporting was requested".into());
    }
    if !t.wrote(b"\x1b[?2004h") {
        return Err("  bracketed paste was not requested".into());
    }
    Ok(())
}

/// Outside a field a paste is dropped: here its `a` would open a question,
/// and the `/` after it would be typed into that instead of opening the
/// filter. Inside a field its first line is typed and the rest is not.
fn a_paste_is_typed_into_a_field_and_dropped_outside_one() -> Result<(), String> {
    let mut t = Term::open("plain", 40, 100)?;
    t.wait("the first frame", |t| t.shows("plain-0"))?;
    t.send("\x1b[200~ad\x1b[201~")?;
    t.send("/")?;
    t.wait("the filter field, and no question", |t| {
        t.footer().starts_with("filter: \u{2588}")
    })?;
    // A terminal sends a pasted line break as a carriage return.
    t.send("\x1b[200~plain-2\rG\x1b[201~")?;
    t.wait("the first pasted line, typed", |t| {
        t.footer().starts_with("filter: plain-2\u{2588}") && t.shows("plain-21")
    })
}

/// A terminal a third of a second away. Its replies used to arrive after the
/// probe had stopped listening, and were read as keys: the `a`s in this
/// colour would open a question, and the `/` would be typed into it.
fn a_late_probe_reply_is_not_typed() -> Result<(), String> {
    let mut t = Term::open_unanswered("plain", 24, 100)?;
    std::thread::sleep(Duration::from_millis(300));
    t.send("\x1b]11;rgb:aaaa/aaaa/aaaa\x07\x1b[?65;1c")?;
    t.wait("the first frame", |t| t.shows("plain-0"))?;
    t.send("/")?;
    t.wait("the filter field, and no question", |t| {
        t.footer().starts_with("filter: \u{2588}")
    })
}

/// Time spent waiting for a key was counted as animation time, so the first
/// frame after a pause had already closed most of the way: a single keypress
/// never drew the brackets at the full spread it had just been given.
fn one_keypress_shows_the_brackets_opening() -> Result<(), String> {
    let mut t = Term::open("plain", 24, 100)?;
    t.wait("the first frame", |t| t.shows("plain-0"))?;
    t.quiet()?;
    let from = t.raw.len();
    t.send("j")?;
    t.wait("the cursor to move", |t| {
        t.bracketed().as_deref() == Some("plain-1")
    })?;
    t.quiet()?;
    // The opening bracket is drawn at column `indent - spread`, counting from
    // one as the escape does, and a depth-0 label is indented 7.
    const INDENT: usize = 7;
    let widest = bracket_columns(&t.raw[from..])
        .into_iter()
        .map(|col| INDENT.saturating_sub(col))
        .max();
    match widest {
        Some(4) => Ok(()),
        other => Err(format!(
            "  the widest spread drawn was {other:?}, not Some(4)"
        )),
    }
}

/// The property the blocking poll exists for.
fn an_idle_screen_writes_nothing() -> Result<(), String> {
    let mut t = Term::open("plain", 24, 100)?;
    t.wait("the first frame", |t| t.shows("plain-0"))?;
    t.quiet()?;
    let before = t.raw.len();
    t.pump(Duration::from_millis(500))?;
    match t.raw.len() - before {
        0 => Ok(()),
        n => Err(format!("  {n} bytes written while idle")),
    }
}

/// The 1-based columns at which a reticle bracket `[` was drawn.
fn bracket_columns(raw: &[u8]) -> Vec<usize> {
    const DRAWN: &[u8] = b"H\x1b[38;5;211m[";
    let mut cols = Vec::new();
    for at in 0..raw.len().saturating_sub(DRAWN.len()) {
        if !raw[at..].starts_with(DRAWN) {
            continue;
        }
        // Back over the digits of `CSI row ; col H` to the column.
        let digits = raw[..at]
            .iter()
            .rev()
            .take_while(|b| b.is_ascii_digit())
            .count();
        if let Ok(col) = String::from_utf8_lossy(&raw[at - digits..at]).parse() {
            cols.push(col);
        }
    }
    cols
}

// ─── The terminal ────────────────────────────────────────────────────────

/// A child on a pty, and what its screen would be showing.
struct Term {
    master: File,
    child: Child,
    grid: Grid,
    /// Everything the child wrote, for the checks that are about bytes.
    raw: Vec<u8>,
    /// Whether to answer the probe, as a terminal does.
    answers: bool,
}

impl Term {
    fn open(screen: &str, rows: u16, cols: u16) -> Result<Self, String> {
        Self::spawn(screen, rows, cols, true).map_err(|e| format!("  could not start: {e}"))
    }

    /// A terminal that says nothing until the check speaks for it.
    fn open_unanswered(screen: &str, rows: u16, cols: u16) -> Result<Self, String> {
        Self::spawn(screen, rows, cols, false).map_err(|e| format!("  could not start: {e}"))
    }

    fn spawn(screen: &str, rows: u16, cols: u16, answers: bool) -> io::Result<Self> {
        let (mut master, mut slave) = (0, 0);
        let mut size = winsize(rows, cols);
        // SAFETY: two valid out-pointers and a valid size; the name and the
        // termios are optional and omitted.
        let opened = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &raw mut size,
            )
        };
        if opened != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `master` was just opened and is owned by nothing else. The
        // flag keeps it out of the child, which must hold only the slave.
        let master = unsafe {
            libc::fcntl(master, libc::F_SETFD, libc::FD_CLOEXEC);
            File::from_raw_fd(master)
        };
        let mut command = Command::new(std::env::current_exe()?);
        command
            .env(SCREEN, screen)
            .env("TERM", "xterm-256color")
            // Inline images are a wezterm escape; nothing here is wezterm.
            .env_remove("TERM_PROGRAM");
        // SAFETY: `login_tty` only makes the slave the child's controlling
        // terminal and its stdio -- setsid, an ioctl, dup2 and close -- which
        // is all that may safely run between fork and exec.
        unsafe {
            command.pre_exec(move || match libc::login_tty(slave) {
                0 => Ok(()),
                _ => Err(io::Error::last_os_error()),
            });
        }
        let child = command.spawn();
        // SAFETY: the parent's copy, which the child no longer needs open.
        unsafe { libc::close(slave) };
        Ok(Self {
            master,
            child: child?,
            grid: Grid::new(rows, cols),
            raw: Vec::new(),
            answers,
        })
    }

    fn send(&mut self, keys: &str) -> Result<(), String> {
        self.master
            .write_all(keys.as_bytes())
            .map_err(|e| format!("  could not type {keys:?}: {e}"))
    }

    fn resize(&mut self, rows: u16, cols: u16) -> Result<(), String> {
        let size = winsize(rows, cols);
        // SAFETY: a valid fd and a valid size. The kernel sends the child its
        // SIGWINCH.
        if unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &size) } != 0 {
            return Err(format!("  resize: {}", io::Error::last_os_error()));
        }
        self.grid.resize(rows, cols);
        Ok(())
    }

    /// Read for up to `wait`. Returns how many bytes arrived.
    fn pump(&mut self, wait: Duration) -> Result<usize, String> {
        let mut fds = libc::pollfd {
            fd: self.master.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one initialised pollfd, count 1.
        if unsafe { libc::poll(&mut fds, 1, wait.as_millis() as i32) } <= 0 {
            return Ok(0);
        }
        let mut chunk = [0u8; 65536];
        // A pty reports the child's exit as an error rather than as an end.
        let n = self.master.read(&mut chunk).unwrap_or(0);
        let chunk = &chunk[..n];
        if self.answers && contains(chunk, b"\x1b[c") {
            self.send("\x1b[?65;1c")?;
        }
        self.raw.extend_from_slice(chunk);
        self.grid.feed(chunk);
        Ok(n)
    }

    /// Read until `ready`, or fail saying what never happened and what the
    /// screen showed instead.
    fn wait(&mut self, what: &str, ready: impl Fn(&Self) -> bool) -> Result<(), String> {
        let start = Instant::now();
        while !ready(self) {
            if start.elapsed() > DEADLINE {
                return Err(format!(
                    "  waited for {what}; the screen shows:\n{}",
                    self.grid
                ));
            }
            if self.pump(Duration::from_millis(50))? == 0 && !self.alive() {
                return Err(format!("  the TUI exited while waiting for {what}"));
            }
        }
        Ok(())
    }

    /// Read until nothing has arrived for a while: every animation is over.
    fn quiet(&mut self) -> Result<(), String> {
        let start = Instant::now();
        while self.pump(Duration::from_millis(250))? > 0 {
            if start.elapsed() > DEADLINE {
                return Err("  the screen never stopped drawing".into());
            }
        }
        Ok(())
    }

    fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    fn shows(&self, text: &str) -> bool {
        self.grid.to_string().contains(text)
    }

    fn wrote(&self, bytes: &[u8]) -> bool {
        contains(&self.raw, bytes)
    }

    fn line(&self, row: usize) -> String {
        self.grid.line(row)
    }

    fn footer(&self) -> String {
        self.line(self.grid.cells.len() - 1).trim().to_string()
    }

    /// What the reticle's brackets are around.
    fn bracketed(&self) -> Option<String> {
        self.grid.to_string().lines().find_map(|l| {
            let (_, rest) = l.split_once("[ ")?;
            let (name, _) = rest.split_once(" ]")?;
            Some(
                name.trim_start_matches(['\u{25b8}', '\u{25be}', ' '])
                    .to_string(),
            )
        })
    }
}

impl Drop for Term {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn winsize(rows: u16, cols: u16) -> libc::winsize {
    libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    }
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// As much of a terminal as this crate draws on: absolute cursor moves, a
/// full clear, and text. Colours and modes are read and dropped.
///
/// Small because the crate is disciplined about what it emits -- every row is
/// positioned with its own `CSI row ; col H` and nothing scrolls -- and
/// measured with the crate's own `width`, so a wide glyph takes the two cells
/// here that the layout gave it.
struct Grid {
    cells: Vec<Vec<char>>,
    row: usize,
    col: usize,
    state: State,
    /// The tail of a chunk that ended part-way through a character.
    partial: Vec<u8>,
}

enum State {
    Text,
    Escape,
    Csi(String),
    Osc,
}

/// The second cell of a wide character.
const CONTINUED: char = '\0';

impl Grid {
    fn new(rows: u16, cols: u16) -> Self {
        Self {
            cells: vec![vec![' '; cols as usize]; rows as usize],
            row: 0,
            col: 0,
            state: State::Text,
            partial: Vec::new(),
        }
    }

    fn resize(&mut self, rows: u16, cols: u16) {
        self.cells.resize(rows as usize, Vec::new());
        for line in &mut self.cells {
            line.resize(cols as usize, ' ');
        }
    }

    fn clear(&mut self) {
        for line in &mut self.cells {
            line.fill(' ');
        }
    }

    fn line(&self, row: usize) -> String {
        let Some(line) = self.cells.get(row) else {
            return String::new();
        };
        let text: String = line.iter().filter(|&&c| c != CONTINUED).collect();
        text.trim_end().to_string()
    }

    fn feed(&mut self, bytes: &[u8]) {
        self.partial.extend_from_slice(bytes);
        let pending = std::mem::take(&mut self.partial);
        let whole = match std::str::from_utf8(&pending) {
            Ok(_) => pending.len(),
            Err(e) => e.valid_up_to(),
        };
        for c in String::from_utf8_lossy(&pending[..whole]).chars() {
            self.take(c);
        }
        // What is left is either the start of a character still arriving or
        // bytes that are no character at all; only the first is worth keeping.
        let rest = &pending[whole..];
        if rest.len() < 4 {
            self.partial = rest.to_vec();
        }
    }

    fn take(&mut self, c: char) {
        match std::mem::replace(&mut self.state, State::Text) {
            State::Text => match c {
                '\x1b' => self.state = State::Escape,
                '\r' => self.col = 0,
                '\n' => self.row = (self.row + 1).min(self.cells.len().saturating_sub(1)),
                c if c.is_control() => {}
                c => self.put(c),
            },
            State::Escape => match c {
                '[' => self.state = State::Csi(String::new()),
                ']' => self.state = State::Osc,
                _ => {}
            },
            State::Csi(mut params) => match c {
                'H' => {
                    let mut at = params.split(';').map(|n| n.parse::<usize>().unwrap_or(1));
                    self.row = at.next().unwrap_or(1).saturating_sub(1);
                    self.col = at.next().unwrap_or(1).saturating_sub(1);
                }
                'J' if params == "2" => self.clear(),
                '\x40'..='\x7e' => {}
                c => {
                    params.push(c);
                    self.state = State::Csi(params);
                }
            },
            // To the bell that ends it. Nothing this crate sends ends an OSC
            // any other way.
            State::Osc if c == '\x07' => {}
            State::Osc => self.state = State::Osc,
        }
    }

    fn put(&mut self, c: char) {
        let wide = tui_core::width::char_width(c);
        if let Some(line) = self.cells.get_mut(self.row) {
            if wide > 0 && self.col + wide <= line.len() {
                line[self.col] = c;
                for cell in &mut line[self.col + 1..self.col + wide] {
                    *cell = CONTINUED;
                }
            }
        }
        self.col += wide;
    }
}

impl std::fmt::Display for Grid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for row in 0..self.cells.len() {
            writeln!(f, "   |{}", self.line(row))?;
        }
        Ok(())
    }
}

// ─── The screens ─────────────────────────────────────────────────────────

/// The child: put up the named screen and run it.
fn show(which: &str) {
    let screen: Box<dyn Screen> = match which {
        "side" => Box::new(Side { mode: 0 }),
        "tabs" => Box::new(Host::new(
            Some("host".into()),
            vec![
                (
                    "One".into(),
                    Box::new(Plain::named("one")) as Box<dyn Screen>,
                ),
                ("Two".into(), Box::new(Plain::named("two"))),
            ],
        )),
        "long" => Box::new(Plain::named(
            "a-label-that-is-much-longer-than-the-left-column-can-possibly-hold",
        )),
        _ => Box::new(Plain::named("plain")),
    };
    if let Err(e) = tui_core::app::run(screen) {
        eprintln!("pty child: {e}");
        std::process::exit(1);
    }
}

/// Two row regions. Picking in the sidebar changes what the list holds.
struct Side {
    mode: usize,
}

impl Side {
    fn tag(&self) -> &'static str {
        ["alpha", "bravo"][self.mode]
    }
}

impl Screen for Side {
    fn title(&self) -> String {
        "side".into()
    }
    fn split(&self) -> Split {
        Split::Sidebar
    }
    fn rows(&mut self) -> Vec<Row> {
        (0..4)
            .map(|i| Row::leaf(format!("{}-{i}", self.tag())))
            .collect()
    }
    fn sidebar(&mut self) -> Vec<Row> {
        vec![Row::leaf("pick-alpha"), Row::leaf("pick-bravo")]
    }
    fn on_sidebar(&mut self, action: Action, sel: usize) -> Flow {
        if action == Action::Activate {
            self.mode = sel;
            return Flow::Dirty;
        }
        Flow::Continue
    }
    fn footer(&self) -> String {
        "side footer".into()
    }
    fn pane(&mut self, _: usize, _: u16, _: u16) -> Pane {
        Pane::default()
    }
    fn on_action(&mut self, action: Action, sel: usize) -> Flow {
        match action {
            // Says which row the SCREEN thinks was opened.
            Action::Activate => Flow::Push(Box::new(Plain::named(format!(
                "opened {}-{sel}",
                self.tag()
            )))),
            _ => Flow::Continue,
        }
    }
}

/// A list and a pane. `d` detaches a child; `a` asks two questions in a row.
struct Plain {
    name: String,
    asked: u8,
    answers: Vec<String>,
}

impl Plain {
    fn named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            asked: 0,
            answers: Vec::new(),
        }
    }
}

impl Screen for Plain {
    fn title(&self) -> String {
        self.name.clone()
    }
    fn rows(&mut self) -> Vec<Row> {
        (0..30)
            .map(|i| Row::leaf(format!("{}-{i}", self.name)))
            .collect()
    }
    fn footer(&self) -> String {
        format!("answers={}", self.answers.join(","))
    }
    fn pane(&mut self, sel: usize, _: u16, _: u16) -> Pane {
        Pane {
            image: None,
            lines: (0..60).map(|i| format!("pane {sel} line {i}")).collect(),
        }
    }
    fn on_action(&mut self, action: Action, _: usize) -> Flow {
        match action {
            // Says when it is running, so a check knows the terminal is the
            // child's before it presses ctrl-c.
            Action::Key('d') => Flow::Detach(vec![
                "sh".into(),
                "-c".into(),
                "echo ready; exec sleep 30".into(),
            ]),
            Action::Key('a') => {
                self.asked = 1;
                Flow::Ask {
                    label: "first".into(),
                    initial: String::new(),
                }
            }
            _ => Flow::Continue,
        }
    }
    fn answered(&mut self, text: Option<String>) -> Flow {
        let Some(text) = text else {
            return Flow::Dirty;
        };
        self.answers.push(text);
        if self.asked == 1 {
            self.asked = 2;
            return Flow::Ask {
                label: "second".into(),
                initial: String::new(),
            };
        }
        Flow::Dirty
    }
}
