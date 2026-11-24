//! The loop: draw, wait, move, repeat.
//!
//! ─── Why this is two redraws and not one ─────────────────────────────────
//!
//! The reticle animates at 60fps. The right-hand pane can contain a PNG that
//! base64s to ~65KB. Re-sending that every frame is 4MB/s down a pty for an
//! animation the image is not part of, which is not a rounding error — it is
//! the difference between smooth and unusable.
//!
//! So the panes are redrawn on different clocks. `draw_rows` runs per frame and
//! writes only the left column. `draw_pane` runs when the SELECTION changes,
//! and is the only thing that ever emits an image. The terminal keeps the
//! pixels in between, which is exactly what an alternate screen is for.
//!
//! ─── Why it blocks ───────────────────────────────────────────────────────
//!
//! When nothing is moving, `poll` waits indefinitely and the process costs
//! nothing. A TUI that spins at 60fps to show a static list is a laptop fan.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyEventKind};
use crossterm::{cursor, queue, style, terminal};

use crate::nav::{Action, Keymap};
use crate::reticle::Reticle;
use crate::screen::{Flow, Pane, Row, Screen, Split, Tabs, Tick};
use crate::term::Terminal;

const FRAME: Duration = Duration::from_millis(16);
/// The most of a paste a field will take. A field is one line holding one
/// identifier; this only stops a pasted novel being typed in a letter a frame.
const PASTE_MAX: usize = 256;
/// Left column width. Wide enough for a long family name, its badge and the
/// brackets at full spread; the pane gets the rest. Adaptive, because a fixed
/// 52 on an 80-column terminal leaves a pane too narrow to hold a sentence,
/// and a fixed one that fits an 80-column terminal truncates every value on a
/// wide one.
fn left_width(cols: u16) -> u16 {
    52.min(cols.saturating_sub(30)).max(24)
}

/// 256-colour indices worth resolving up front: the ones this repo's PS1
/// actually uses (75 for user@host, 110 for the path), plus the greens and
/// reds a specimen tends to want.
const PALETTE_WANTED: &[u8] = &[75, 110, 108, 114, 210, 221, 244, 250];

pub fn run(root: Box<dyn Screen>) -> io::Result<()> {
    run_stack(vec![root])
}

/// Start with screens ALREADY pushed, topmost last.
///
/// So a command can open where you are standing without that becoming a mode.
/// `github` run inside a repo seeds `[root, org, repo]` and shows the repo; `q`
/// then walks up to the org and the root and quits at it, which is what this
/// function already did for screens pushed by hand. The alternative -- making
/// the repo the root when the cwd is one -- leaves the org and the rest of the
/// account unreachable without leaving the program.
///
/// Screens below the top are NOT asked for rows or a pane until they are
/// reached, so a seeded path costs nothing for the levels skipped past. That is
/// load-bearing rather than incidental: a screen that fetched on construction
/// would make the seeded case slower than the unseeded one.
pub fn run_stack(screens: Vec<Box<dyn Screen>>) -> io::Result<()> {
    let mut term = Terminal::take()?;
    // After raw mode, before anything is drawn: the replies are only readable
    // once the terminal has stopped line-buffering, and they would otherwise
    // land in the middle of the first frame.
    crate::set_term_info(crate::probe::probe(PALETTE_WANTED));
    let res = event_loop(screens, &mut term);
    term.restore()?;
    res
}

/// One entry per screen on the stack. The cursor, the scroll offset and the
/// reticle travel WITH the screen, so coming back lands you on the row you
/// left rather than at the top — the same property the fzf implementation got
/// from running one fzf inside another.
struct Frame {
    screen: Box<dyn Screen>,
    sel: usize,
    top: usize,
    reticle: Reticle,
    /// Which region hjkl is acting on. Travels with the screen, so coming back
    /// to one lands you where you were in every sense.
    focus: Focus,
    /// First pane line drawn, for when the pane has focus and more lines than
    /// fit.
    pane_top: usize,
    /// The sidebar's own cursor, for `Split::Sidebar`. Separate because the
    /// two lists are navigated separately and crossing between them must not
    /// move the cursor you are coming back to.
    side_sel: usize,
    side_top: usize,
    /// The sidebar's own brackets, for the same reason it has its own cursor.
    /// Sharing the list's put them on whichever sidebar row had the LIST
    /// cursor's number, or on none.
    side_reticle: Reticle,
    /// What the MAIN list is narrowed to -- the mock's search bar. The sidebar
    /// is a handful of rows and has nothing to filter.
    ///
    /// Persistent per screen, like the focus and the cursor, so coming back to
    /// a filtered list finds it as you left it.
    filter: String,
    /// The text field, when one has the keyboard.
    edit: Option<Edit>,
    /// False until the screen has been asked where to open. A screen seeded
    /// underneath the top one is asked when it is first popped to, not before.
    opened: bool,
}

impl Frame {
    fn new(screen: Box<dyn Screen>) -> Self {
        Self {
            screen,
            sel: 0,
            top: 0,
            reticle: Reticle::new(0),
            focus: Focus::List,
            pane_top: 0,
            side_sel: 0,
            side_top: 0,
            side_reticle: Reticle::new(0),
            filter: String::new(),
            edit: None,
            opened: false,
        }
    }

    /// Put the cursor where the screen asks to open, with nothing animating.
    ///
    /// The start, a push, a `Flow::Retarget` and the first pop to a seeded
    /// screen all come through here. A push used to hard-code row 0, which was
    /// invisible for as long as nothing overrode `initial_sel` and then
    /// silently wrong for the first screen that did: `github` drilling into a
    /// failed run opened on the run's first job instead of on the step that
    /// failed.
    fn open(&mut self, rows: &[Row]) {
        let vis = crate::filter::visible(rows, &self.filter);
        self.sel = start_sel(self.screen.as_ref(), rows, &vis);
        self.top = 0;
        self.pane_top = 0;
        self.reticle = Reticle::new(self.sel);
        if let Some(at) = under_cursor(rows, &vis, self.sel) {
            self.screen.focus(at);
        }
        if let Some(row) = self.screen.initial_side_sel() {
            self.side_sel = row;
            self.side_top = 0;
            self.side_reticle = Reticle::new(row);
        }
        self.opened = true;
    }
}

/// A text field with the keyboard.
struct Edit {
    what: Asking,
    text: String,
}

enum Asking {
    /// Narrowing the list. Every keystroke applies, so the list moves under
    /// you as you type -- which is what fzf did and what makes it feel like
    /// filtering rather than like a query box.
    Filter {
        /// The ROW the cursor was on, to put back if this is abandoned. A row
        /// rather than a position, because the position means something else
        /// the moment the filter changes.
        was: usize,
    },
    /// A value the SCREEN asked for, with its prompt.
    Screen(String),
}

/// What is left to do once a screen's answer has been applied to its frame.
enum Outcome {
    /// `Flow::Continue`: the screen did nothing with the key. For a direction
    /// that is the door to the next region.
    Nothing,
    /// Applied. Redraw.
    Applied,
    Quit,
    Pop,
    Push(Box<dyn Screen>),
}

/// Which region the keys are acting on.
///
/// ─── Why directional focus instead of a key per region ──────────────────
///
/// `Tab` used to be the only way to reach the strip, and a key that exists
/// solely to change what the other keys mean is a mode -- the thing this nav
/// contract is built to avoid. Riley's word for the fix was a smart TV, and it
/// is the right model: hjkl moves you around the screen, and the EDGE of a
/// region is the door to the next one.
///
/// The rule that makes it work without the screens knowing: a direction acts
/// within the focused region, and when it cannot -- the cursor is at the top
/// row, the tree node has nothing to collapse -- focus moves instead. For
/// `h`/`l` that is read straight off `Flow::Continue`, which already means
/// "nothing happened"; no screen had to change to take part.
///
/// `Tab` is bound to nothing: see `nav::Action::NextTab`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Focus {
    /// The header strip. `h`/`l` change tab, `j` drops into the body.
    Tabs,
    /// The left column of a `Split::Sidebar` screen: its filters and
    /// controls. `l` crosses to the list, `k` off the top reaches the strip.
    Sidebar,
    /// The row list. The default, and where everything used to happen.
    List,
    /// The right-hand pane of a `Split::ListPane` screen. `j`/`k` scroll it,
    /// `h` goes back to the list.
    Pane,
}

/// Run `argv` with the terminal handed back, then take it again.
///
/// The child's status is deliberately ignored: a failed `make check` is
/// information, not an error for this process, and the user has just watched
/// the output scroll past. What matters is that the terminal comes back even
/// if the child dies badly -- hence resume() on every path.
///
/// In `dir` if one is given, and otherwise wherever this process is.
fn detach(
    term: &mut Terminal,
    out: &mut impl Write,
    argv: &[String],
    dir: Option<&Path>,
) -> io::Result<()> {
    let Some((program, args)) = argv.split_first() else {
        return Ok(());
    };
    term.suspend()?;
    let status = {
        let _child_owns_ctrl_c = ChildOwnsCtrlC::new();
        let mut child = Command::new(program);
        child.args(args);
        if let Some(dir) = dir {
            child.current_dir(dir);
        }
        child.status()
    };
    // A beat to read the last line before the alternate screen swallows it.
    let _ = writeln!(out);
    let _ = match &status {
        Ok(s) if s.success() => writeln!(out, "  done — enter to go back"),
        Ok(s) => match s.code() {
            Some(code) => writeln!(out, "  exited {code} — enter to go back"),
            None => writeln!(out, "  interrupted — enter to go back"),
        },
        Err(e) => writeln!(out, "  could not run {program}: {e} — enter to go back"),
    };
    let _ = out.flush();
    // Read on the RESTORED terminal, so this is a line-buffered wait rather
    // than a raw keypress. Resuming first would swap the screen out from under
    // the output the user is being asked to read.
    let mut sink = String::new();
    let _ = io::stdin().read_line(&mut sink);
    term.resume()
}

/// While a detached child runs, ctrl-c and ctrl-\ are the CHILD's.
///
/// The tty is cooked again by then, so both keys signal the whole foreground
/// process group -- this process included, and its default answer is to die.
/// Stopping `gh run watch` took the TUI with it.
///
/// A handler that does nothing, rather than `SIG_IGN`: an ignored signal stays
/// ignored across `exec`, so the child could not be interrupted either, while a
/// handler is reset to the default there. The old dispositions come back on
/// drop, so ctrl-c at the "enter to go back" prompt still leaves.
struct ChildOwnsCtrlC {
    int: libc::sighandler_t,
    quit: libc::sighandler_t,
}

extern "C" fn stay(_: libc::c_int) {}

impl ChildOwnsCtrlC {
    fn new() -> Self {
        let stay = stay as extern "C" fn(libc::c_int) as libc::sighandler_t;
        // SAFETY: `stay` does nothing, which is async-signal-safe, and it is
        // a valid handler for the life of the process.
        unsafe {
            Self {
                int: libc::signal(libc::SIGINT, stay),
                quit: libc::signal(libc::SIGQUIT, stay),
            }
        }
    }
}

impl Drop for ChildOwnsCtrlC {
    fn drop(&mut self) {
        // SAFETY: both values are dispositions `signal` itself returned.
        unsafe {
            libc::signal(libc::SIGINT, self.int);
            libc::signal(libc::SIGQUIT, self.quit);
        }
    }
}

/// Apply what a screen answered, whichever of its handlers answered.
///
/// One function because there were four copies of this match, one per place a
/// screen is asked something, and they had drifted: a sidebar pick returned
/// `Flow::Dirty` and the list beside it was never re-read, so `enter` opened a
/// row other than the one on screen; and a question answered with another
/// question never opened the second field. A `Flow` now means the same thing
/// wherever it comes from.
fn apply(
    flow: Flow,
    frame: &mut Frame,
    rows: &mut Vec<Row>,
    term: &mut Terminal,
    out: &mut impl Write,
) -> io::Result<Outcome> {
    match flow {
        Flow::Continue => return Ok(Outcome::Nothing),
        Flow::Quit => return Ok(Outcome::Quit),
        Flow::Pop => return Ok(Outcome::Pop),
        Flow::Push(next) => return Ok(Outcome::Push(next)),
        Flow::Dirty => *rows = frame.screen.rows(),
        Flow::Detach(argv) => {
            detach(term, out, &argv, None)?;
            *rows = frame.screen.rows();
        }
        Flow::DetachIn { dir, argv } => {
            detach(term, out, &argv, Some(&dir))?;
            *rows = frame.screen.rows();
        }
        Flow::Ask { label, initial } => {
            frame.edit = Some(Edit {
                what: Asking::Screen(label),
                text: initial,
            });
        }
        // Every row was replaced, so clamping the old index would land the
        // cursor somewhere arbitrary. The screen is asked where to open,
        // exactly as it is on a push.
        Flow::Retarget => {
            *rows = frame.screen.rows();
            frame.open(rows);
        }
    }
    Ok(Outcome::Applied)
}

fn event_loop(screens: Vec<Box<dyn Screen>>, term: &mut Terminal) -> io::Result<()> {
    let mut out = io::stdout();
    // A frame is assembled here and written once. Queued straight into stdout
    // it left in a thousand small writes through a 1KB buffer, so the terminal
    // could paint between the clear and what was drawn over it.
    //
    // A `Vec` and not a `BufWriter`: a panic restores the terminal first and
    // THEN unwinds, and a `BufWriter` dropped on the way out would flush its
    // half-drawn frame over the panic message. A `Vec` drops silently.
    let mut buf: Vec<u8> = Vec::new();
    // What is left of a paste, still to be typed into the field it landed in.
    let mut pasted: VecDeque<char> = VecDeque::new();
    let mut keys = Keymap::default();
    let mut stack: Vec<Frame> = screens.into_iter().map(Frame::new).collect();
    // Only the TOP screen is focused and asked for rows. The ones underneath
    // are untouched until popped to, which is what keeps a seeded path free.
    // With no screens at all there is nothing to show.
    let Some(first) = stack.last_mut() else {
        return Ok(());
    };
    let mut rows = first.screen.rows();
    first.open(&rows);
    let mut last = Instant::now();
    let mut pane_dirty = true;
    let mut busy = false;
    let mut pane_lines = 0usize;
    let mut pane_room = 0usize;

    loop {
        let depth = stack.len();
        let frame = stack.last_mut().expect("stack is never empty");
        // `frame.sel` is a POSITION in `vis`, never a row index. The screen is
        // handed `vis[sel]`, which is ITS index -- the translation happens at
        // this boundary and nowhere else, which is why no screen knows a
        // filter exists and why every screen already written gained filtering
        // without a line changing in it.
        let mut vis = crate::filter::visible(&rows, &frame.filter);
        // Wherever these rows came from -- a key, a refresh that landed, a
        // group that closed -- the cursor rests on one that can be selected.
        // A screen that answered `Flow::Dirty` after closing a group used to
        // leave it on the blank row that took the place of what it was on.
        let rested = settled(&rows, &vis, frame.sel);
        if rested != frame.sel {
            frame.sel = rested;
            // And the screen is told, as it is when a key moves the cursor:
            // what it believes about where the cursor is has to stay true.
            if let Some(at) = under_cursor(&rows, &vis, rested) {
                frame.screen.focus(at);
            }
            pane_dirty = true;
        }
        // `None` when a filter hides every row. There is nothing to act ON
        // then, and a key sent with a made-up row 0 would act on a row nobody
        // can see -- so nothing is sent, and no pane is drawn, until something
        // is showing again.
        let cur = under_cursor(&rows, &vis, frame.sel);
        let (cols, screen_rows) = Terminal::size()?;
        let split = frame.screen.split();
        let full = matches!(split, Split::Full);
        let sided = matches!(split, Split::Sidebar);
        // Only `ListPane` has a pane. A `Sidebar` screen's right-hand region
        // is its list, so there is nothing for `l` to move into from there.
        let paned = matches!(split, Split::ListPane);
        // The sidebar's rows, and its own visible list. No filter: the search
        // bar in the mock belongs to the main list, and a column of five
        // filters has nothing to narrow.
        let side: Vec<Row> = if sided {
            frame.screen.sidebar()
        } else {
            Vec::new()
        };
        let svis: Vec<usize> = (0..side.len()).collect();
        if frame.side_sel >= svis.len() {
            frame.side_sel = svis.len().saturating_sub(1);
        }
        // The brackets chase their cursor from HERE, once per frame, rather
        // than from each place a cursor can move. A cursor moved by a clamp
        // above -- a refresh that shortened the list -- used to leave them
        // aimed at a row that was gone. Aiming where they already point does
        // nothing.
        frame.reticle.aim(frame.sel);
        frame.side_reticle.aim(frame.side_sel);
        // `Full` gives the whole width to the list; there is no divider to
        // leave room for and no pane to leave room beside.
        let left = if full { cols } else { left_width(cols) };
        let tabs = frame.screen.tabs();
        // Each region says what ITS keys do. The screen only speaks for the
        // list, because that is the only region whose contents it owns -- and a
        // footer that still advertised `enter runs it` while the keys were
        // talking to the pane would be the kind of small lie that gets
        // believed.
        let foot = match &frame.edit {
            // The field IS the footer. A separate line for it would cost a row
            // on every screen to serve the one moment something is being
            // typed, and the footer is already where this program answers
            // "what do the keys do now".
            Some(e) => {
                let (label, hint) = match &e.what {
                    Asking::Filter { .. } if vis.is_empty() => ("filter", "  (nothing matches)"),
                    Asking::Filter { .. } => (
                        "filter",
                        "  \u{2191}\u{2193} move \u{b7} enter keep \u{b7} esc clear",
                    ),
                    Asking::Screen(l) => (l.as_str(), "  enter accept \u{b7} esc cancel"),
                };
                format!("{label}: {}\u{2588}{hint}", e.text)
            }
            None => {
                let region = match frame.focus {
                    Focus::Sidebar => {
                        "j/k move \u{b7} enter pick \u{b7} l to the list \u{b7} k at the top for the tabs".into()
                    }
                    Focus::List => frame.screen.footer(),
                    Focus::Tabs => {
                        "h/l pick a tab \u{b7} j into it \u{b7} enter takes you there".into()
                    }
                    // The position, not the visible range: the range would need the
                    // pane height, which comes from the layout, which needs this
                    // string's line count. Circular for no gain.
                    Focus::Pane => format!(
                        "j/k scroll \u{b7} ctrl-d/u page \u{b7} h back to the list \u{b7} line {} of {}",
                        frame.pane_top + 1,
                        pane_lines
                    ),
                };
                if frame.filter.is_empty() {
                    region
                } else {
                    // Said out loud, because a list that is quietly hiding rows is
                    // the worst thing a filter can do -- you look for something,
                    // it is not there, and nothing says why. Added to whichever
                    // region's keys are showing, not put in place of them.
                    format!(
                        "{region} \u{b7} filtering on \"{}\" \u{b7} / to change \u{b7} esc to clear",
                        frame.filter
                    )
                }
            }
        };
        let geo = layout(screen_rows, tabs.is_some(), footer_lines(&foot));
        let list_h = geo.list_h;

        // Keep each cursor on screen.
        frame.top = scrolled(frame.top, frame.sel, list_h);
        frame.side_top = scrolled(frame.side_top, frame.side_sel, list_h);

        if pane_dirty {
            let ph = screen_rows.saturating_sub(geo.header_h + geo.footer_h);
            // Built BEFORE the frame is cleared only when there is one -- a
            // `Full` or `Sidebar` screen is never asked for a pane at all, so
            // it does not have to implement one it would never draw.
            let pane = paned.then(|| {
                cur.map_or_else(Pane::default, |row| {
                    frame.screen.pane(row, cols.saturating_sub(left + 1), ph)
                })
            });
            draw_frame(
                &mut buf,
                frame.screen.as_mut(),
                &Chrome {
                    tabs: tabs.as_ref(),
                    focus: frame.focus,
                    cols,
                    rows: screen_rows,
                    left: if full { None } else { Some(left) },
                    geo: &geo,
                },
            )?;
            pane_lines = pane.as_ref().map(|p| p.lines.len()).unwrap_or(0);
            // How far the pane can scroll: what it holds, less what fits.
            pane_room = pane_lines.saturating_sub(ph as usize);
            if frame.pane_top > pane_room {
                frame.pane_top = pane_room;
            }
            if let Some(pane) = &pane {
                draw_pane(
                    &mut buf,
                    pane,
                    left + 1,
                    geo.header_h,
                    cols.saturating_sub(left + 1),
                    ph,
                    frame.pane_top,
                )?;
            }
            pane_dirty = false;
        }
        if sided {
            // Left: the filters. Right: the list. Both are real lists with
            // their own cursor, and only the focused one wears the brackets --
            // two sets on screen would be two answers to "where am I".
            draw_rows(
                &mut buf,
                &side,
                &svis,
                &View {
                    x0: 0,
                    top: frame.side_top,
                    sel: frame.side_sel,
                    reticle: (frame.focus == Focus::Sidebar).then_some(&frame.side_reticle),
                },
                left,
                geo.header_h,
                list_h,
            )?;
        }
        // The list itself: the left column, or the right of a sidebar, or the
        // whole width.
        let (x0, right) = if sided { (left + 1, cols) } else { (0, left) };
        draw_rows(
            &mut buf,
            &rows,
            &vis,
            &View {
                x0,
                top: frame.top,
                sel: frame.sel,
                // Only the focused region wears the brackets.
                reticle: (frame.focus == Focus::List).then_some(&frame.reticle),
            },
            right,
            geo.header_h,
            list_h,
        )?;
        draw_footer(&mut buf, &foot, cols, screen_rows, depth, geo.footer_h)?;
        out.write_all(&buf)?;
        out.flush()?;
        buf.clear();

        let mut wake: Option<Duration> = None;
        match frame.screen.tick() {
            Tick::Idle => {}
            Tick::Busy => busy = true,
            Tick::Wake(d) => wake = Some(d),
            Tick::Changed => {
                busy = true;
                pane_dirty = true;
                rows = frame.screen.rows();
                continue;
            }
        }
        // While a field has the keyboard, bare letters are text. Set from the
        // frame on every iteration rather than tracked alongside it, so a
        // push, a pop, a detach or a tab switch cannot leave the keymap in the
        // wrong mode -- which is the one failure here that would be silent and
        // total: every single-key action firing while you tried to type.
        keys.typing(frame.edit.is_some());

        let animating = frame.reticle.moving() || frame.side_reticle.moving();
        let timeout = poll_timeout(animating || busy, wake);
        busy = false;

        if frame.edit.is_none() {
            pasted.clear();
        }
        // Nothing is waited for while some of a paste is left to type.
        let ev = if !pasted.is_empty() {
            None
        } else if event::poll(timeout)? {
            Some(event::read()?)
        } else {
            None
        };
        // A resize moves every region, and only a full redraw puts the
        // divider, the pane and the footer where the new size says they go.
        if matches!(ev, Some(Event::Resize(..))) {
            pane_dirty = true;
        }
        let action = match ev {
            Some(Event::Key(k)) if k.kind == KeyEventKind::Press => keys.resolve(k),
            // A paste arrives WHOLE, which is the only thing that tells it
            // from typing. Outside a field it is dropped: every letter of it
            // would otherwise be a single-key action, and a stray middle
            // click is a screenful of those. Into a field it is typed, a
            // character per turn of the loop, so it narrows a list exactly as
            // the same letters from the keyboard would.
            //
            // The first line only, and a bounded amount of it: a field holds
            // one identifier, and a newline would have meant "accept". Split
            // on `\r` as well, which is what a terminal sends for a pasted
            // line break.
            Some(Event::Paste(text)) if frame.edit.is_some() => {
                let line = text.split(['\r', '\n']).next().unwrap_or_default();
                pasted.extend(line.chars().filter(|c| !c.is_control()).take(PASTE_MAX));
                pasted.pop_front().map(Action::Type)
            }
            _ => pasted.pop_front().map(Action::Type),
        };

        let mut outcome = Outcome::Nothing;
        if let Some(action) = action {
            let before = frame.sel;
            // A region gaining or losing the keys, or the pane scrolling, is a
            // redraw of the chrome. Read off the state afterwards rather than
            // set at each of the places either can change, one of which had
            // been missed.
            let was = (frame.focus, frame.pane_top);

            // A field has the keyboard: everything but motion is text.
            if let Some(edit) = frame.edit.as_mut() {
                let mut done: Option<Option<String>> = None;
                match action {
                    Action::Type(c) => edit.text.push(c),
                    Action::Backspace => {
                        edit.text.pop();
                    }
                    Action::Clear => edit.text.clear(),
                    Action::Accept => done = Some(Some(edit.text.clone())),
                    Action::Cancel => done = Some(None),
                    Action::Quit => return Ok(()),
                    _ => {}
                }
                // A filter applies on every keystroke, so the list moves
                // under you as you type -- which is what made fzf feel
                // like filtering rather than like a query box. A screen's
                // question does not exist until it is accepted.
                let narrowed =
                    matches!(edit.what, Asking::Filter { .. }) && edit.text != frame.filter;
                if narrowed {
                    frame.filter.clone_from(&edit.text);
                    vis = crate::filter::visible(&rows, &frame.filter);
                    // Land on the match, not on the heading above it.
                    frame.sel = crate::filter::first_leaf(&rows, &vis);
                } else {
                    // Arrows move through the filtered list without leaving
                    // the field: type a bit, arrow down, enter.
                    match action {
                        Action::Down => frame.sel = seek(&rows, &vis, frame.sel, 1),
                        Action::Up => frame.sel = seek(&rows, &vis, frame.sel, -1),
                        _ => {}
                    }
                }
                if let Some(answer) = done {
                    match frame.edit.take().map(|e| e.what) {
                        // Abandoned: the list comes back whole and the cursor
                        // returns to the row it was on. With nothing hidden a
                        // row's index IS its position.
                        Some(Asking::Filter { was }) if answer.is_none() => {
                            frame.filter.clear();
                            frame.sel = was;
                        }
                        Some(Asking::Screen(_)) => {
                            let flow = frame.screen.answered(answer);
                            outcome = apply(flow, frame, &mut rows, term, &mut out)?;
                            // Whatever it answered, the screen was just told
                            // something that may have changed its rows.
                            rows = frame.screen.rows();
                        }
                        _ => {}
                    }
                    vis = crate::filter::visible(&rows, &frame.filter);
                }
                let at = frame.sel.min(vis.len().saturating_sub(1));
                frame.sel = first_selectable(&rows, &vis, at, -1);
                pane_dirty = true;
            } else {
                match action {
                    // At the root this leaves; anywhere else it goes back one
                    // screen, which is what every other pane-and-list tool in
                    // this repo trained your fingers to expect.
                    Action::Quit => {
                        // A filter in force is cleared FIRST. Without this,
                        // `esc` on a narrowed list throws away the screen you
                        // were on -- a surprising amount to lose for a key
                        // pressed meaning "show me everything again" -- and
                        // the footer says it clears.
                        if !frame.filter.is_empty() {
                            frame.filter.clear();
                            // The cursor stays on the ROW it was on: with
                            // nothing hidden, that row's index is its position.
                            frame.sel = cur.unwrap_or(0);
                            frame.reticle = Reticle::new(frame.sel);
                            pane_dirty = true;
                        } else if depth == 1 {
                            return Ok(());
                        } else {
                            outcome = Outcome::Pop;
                        }
                    }
                    // ── j: down, or out of the strip into the list ──
                    Action::Down => match frame.focus {
                        Focus::Tabs => {
                            // Into whichever region the body starts with.
                            frame.focus = if sided { Focus::Sidebar } else { Focus::List };
                        }
                        Focus::Sidebar => frame.side_sel = seek(&side, &svis, frame.side_sel, 1),
                        // Every motion lands on a SELECTABLE row. Spacers exist
                        // to group things visually; a cursor that can rest in
                        // one turns them into potholes.
                        Focus::List => frame.sel = seek(&rows, &vis, frame.sel, 1),
                        Focus::Pane => frame.pane_top = (frame.pane_top + 1).min(pane_room),
                    },
                    // ── k: up, and off the top row into the strip ──
                    Action::Up => match frame.focus {
                        Focus::Tabs => {}
                        Focus::Sidebar => {
                            let next = seek(&side, &svis, frame.side_sel, -1);
                            if next == frame.side_sel && tabs.is_some() {
                                frame.focus = Focus::Tabs;
                            } else {
                                frame.side_sel = next;
                            }
                        }
                        Focus::List => {
                            let next = seek(&rows, &vis, frame.sel, -1);
                            if next == frame.sel && tabs.is_some() {
                                // Already at the first selectable row, so `k`
                                // had nothing to do here -- that is the door.
                                frame.focus = Focus::Tabs;
                            } else {
                                frame.sel = next;
                            }
                        }
                        Focus::Pane => frame.pane_top = frame.pane_top.saturating_sub(1),
                    },
                    // ── gg, G, ctrl-d, ctrl-u: the same four jumps in whichever
                    // region has the keys ──
                    Action::Top | Action::Bottom | Action::PageDown | Action::PageUp => {
                        let half = list_h / 2;
                        match frame.focus {
                            Focus::Sidebar => {
                                frame.side_sel = jump(&side, &svis, frame.side_sel, action, half);
                            }
                            Focus::Pane => {
                                frame.pane_top = match action {
                                    Action::Top => 0,
                                    Action::Bottom => pane_room,
                                    Action::PageDown => (frame.pane_top + half).min(pane_room),
                                    _ => frame.pane_top.saturating_sub(half),
                                };
                            }
                            Focus::List | Focus::Tabs => {
                                // From the strip, `gg` and `G` go INTO the
                                // list; a page only moves the cursor in it.
                                if matches!(action, Action::Top | Action::Bottom) {
                                    frame.focus = Focus::List;
                                }
                                frame.sel = jump(&rows, &vis, frame.sel, action, half);
                            }
                        }
                    }
                    // `/` narrows the list. Framework-owned, so it works on
                    // every screen including the ones written before it
                    // existed -- and from every region, which is why it sits
                    // above the arms that hand a key to the sidebar.
                    Action::Find => {
                        frame.edit = Some(Edit {
                            what: Asking::Filter {
                                was: cur.unwrap_or(0),
                            },
                            text: frame.filter.clone(),
                        });
                        frame.focus = Focus::List;
                        pane_dirty = true;
                    }
                    // ── h: collapse, or leftwards out of a region ──
                    Action::Close if frame.focus == Focus::Pane => frame.focus = Focus::List,
                    // In the sidebar, `h` is the screen's -- to collapse a
                    // dropdown. There is nothing further left to go to.
                    Action::Close if frame.focus == Focus::Sidebar => {
                        let flow = frame.screen.on_sidebar(Action::Close, frame.side_sel);
                        outcome = apply(flow, frame, &mut rows, term, &mut out)?;
                    }
                    // On a sidebar screen the main list's left edge IS the
                    // sidebar, and the screen gets first refusal: `h`
                    // collapses a group if there is one, and crosses if not.
                    Action::Close if sided && frame.focus == Focus::List => {
                        let flow = on_row(frame, Action::Close, cur);
                        outcome = apply(flow, frame, &mut rows, term, &mut out)?;
                        if matches!(outcome, Outcome::Nothing) {
                            frame.focus = Focus::Sidebar;
                        }
                    }
                    Action::Open if sided && frame.focus == Focus::Sidebar => {
                        let flow = frame.screen.on_sidebar(Action::Open, frame.side_sel);
                        outcome = apply(flow, frame, &mut rows, term, &mut out)?;
                        if matches!(outcome, Outcome::Nothing) {
                            frame.focus = Focus::List;
                        }
                    }
                    // Everything else a sidebar row might do -- enter to
                    // pick, a letter key -- is the screen's other handler.
                    other if frame.focus == Focus::Sidebar => {
                        let flow = frame.screen.on_sidebar(other, frame.side_sel);
                        outcome = apply(flow, frame, &mut rows, term, &mut out)?;
                    }
                    // In the strip, left and right ARE the tabs.
                    Action::Close | Action::Open if frame.focus == Focus::Tabs => {
                        let to = if action == Action::Open {
                            Action::NextTab
                        } else {
                            Action::PrevTab
                        };
                        // Not about a row, so it is sent even when none shows.
                        let flow = frame.screen.on_action(to, cur.unwrap_or(0));
                        if matches!(flow, Flow::Retarget) {
                            // The filter and the sidebar cursor describe the
                            // tab being LEFT. Carried across, a filter typed
                            // on one tab emptied the next.
                            frame.filter.clear();
                            frame.side_sel = 0;
                            frame.side_top = 0;
                            frame.side_reticle = Reticle::new(0);
                        }
                        outcome = apply(flow, frame, &mut rows, term, &mut out)?;
                    }
                    // Enter on the strip commits the tab and drops into it,
                    // which is what a smart TV does and what the hand expects.
                    Action::Activate if frame.focus == Focus::Tabs => frame.focus = Focus::List,
                    other => {
                        let flow = on_row(frame, other, cur);
                        outcome = apply(flow, frame, &mut rows, term, &mut out)?;
                        // The screen did nothing with it. For `l` that is the
                        // door to the pane.
                        //
                        // Whenever a pane EXISTS, not only when it overflows.
                        // Gating on scrollability was the first attempt and it
                        // reads as a dead key: on a run whose body happens to
                        // fit, `l` silently did nothing, which is exactly the
                        // "why won't it move" the directional model is supposed
                        // to remove. The divider lights up and the footer
                        // changes, so the move is visible even when there is
                        // nothing to scroll once you are there.
                        if matches!(outcome, Outcome::Nothing)
                            && other == Action::Open
                            && paned
                            && cur.is_some()
                        {
                            frame.focus = Focus::Pane;
                        }
                    }
                }
            }
            if (frame.focus, frame.pane_top) != was {
                pane_dirty = true;
            }

            // The rows or the filter may have changed under the cursor, so
            // where it IS gets read again before the screen is told.
            let vis = crate::filter::visible(&rows, &frame.filter);
            frame.sel = frame.sel.min(vis.len().saturating_sub(1));
            let at = under_cursor(&rows, &vis, frame.sel);
            if frame.sel != before || at != cur {
                // Separate from `pane` on purpose: a row you skim past
                // still gets prefetched, which is the whole point.
                if let Some(at) = at {
                    frame.screen.focus(at);
                }
                // A scroll offset belongs to the row it was scrolled on.
                frame.pane_top = 0;
                pane_dirty = true;
            }
        }

        // Only time spent ANIMATING moves the brackets. The wait for a key is
        // not animation time: counting it spent four frames of travel on the
        // first frame after every pause, so a single keypress never showed the
        // spread it was given.
        let now = Instant::now();
        if animating {
            let dt = now.duration_since(last);
            frame.reticle.step(dt);
            frame.side_reticle.step(dt);
        }
        last = now;

        match outcome {
            Outcome::Nothing => {}
            Outcome::Applied => pane_dirty = true,
            Outcome::Quit => return Ok(()),
            Outcome::Push(next) => {
                let mut pushed = Frame::new(next);
                rows = pushed.screen.rows();
                pushed.open(&rows);
                stack.push(pushed);
                pane_dirty = true;
            }
            // The root is never popped: a screen asking to close it is asking
            // for nothing.
            Outcome::Pop if depth > 1 => {
                stack.pop();
                let back = stack.last_mut().expect("root is never popped");
                rows = back.screen.rows();
                if !back.opened {
                    back.open(&rows);
                }
                pane_dirty = true;
            }
            Outcome::Pop => {}
        }
    }
}

/// The row under the cursor, in the SCREEN's numbering.
///
/// `None` when rows exist and a filter hides all of them. A list with no rows
/// at all still answers 0, which is what a screen with nothing in it has
/// always been handed.
fn under_cursor(rows: &[Row], vis: &[usize], sel: usize) -> Option<usize> {
    vis.get(sel).copied().or(rows.is_empty().then_some(0))
}

/// Send the screen a key about the row under the cursor -- or send nothing,
/// when no row is.
fn on_row(frame: &mut Frame, action: Action, cur: Option<usize>) -> Flow {
    cur.map_or(Flow::Continue, |row| frame.screen.on_action(action, row))
}

/// Where `gg`, `G`, ctrl-d and ctrl-u put a cursor: the ends, or half a
/// screen away, and always on something selectable.
fn jump(rows: &[Row], vis: &[usize], sel: usize, action: Action, half: usize) -> usize {
    let last = vis.len().saturating_sub(1);
    match action {
        Action::Top => first_selectable(rows, vis, 0, 1),
        Action::Bottom => first_selectable(rows, vis, last, -1),
        Action::PageDown => first_selectable(rows, vis, (sel + half).min(last), -1),
        Action::PageUp => first_selectable(rows, vis, sel.saturating_sub(half), 1),
        _ => sel,
    }
}

/// The scroll offset that keeps `sel` among the `height` rows drawn.
fn scrolled(top: usize, sel: usize, height: usize) -> usize {
    if sel < top {
        sel
    } else if height > 0 && sel >= top + height {
        sel + 1 - height
    } else {
        top
    }
}

/// Everything about the frame that is not the screen itself.
///
/// Bundled because it reached nine arguments once focus became a thing the
/// chrome had to show -- which is a signal rather than a lint. The strip, the
/// divider and the geometry are one description of "the frame around the
/// content", so they travel as one.
struct Chrome<'a> {
    tabs: Option<&'a Tabs>,
    focus: Focus,
    cols: u16,
    rows: u16,
    /// Where the vertical divider goes, or `None` for a full-width list.
    left: Option<u16>,
    geo: &'a Layout,
}

fn draw_frame(out: &mut impl Write, screen: &mut dyn Screen, c: &Chrome) -> io::Result<()> {
    let Chrome {
        tabs,
        focus,
        cols,
        rows,
        left,
        geo,
    } = *c;
    let tabs_focused = focus == Focus::Tabs;
    let pane_focused = focus == Focus::Pane;
    queue!(
        out,
        terminal::Clear(terminal::ClearType::All),
        cursor::MoveTo(0, 0)
    )?;
    match tabs {
        None => {
            let title = screen.title();
            let title = crate::width::clean(&title);
            queue!(out, style::Print(format!("\x1b[1m{title}\x1b[0m")))?;
        }
        Some(t) => {
            queue!(out, style::Print(tab_strip(t, cols, tabs_focused)))?;
            // The rule spans the FULL width, above both columns -- the strip
            // belongs to the screen, not to the list, so the divider starts
            // under it rather than beside it.
            queue!(
                out,
                cursor::MoveTo(0, 1),
                style::Print(fg(240, "\u{2500}".repeat(cols as usize)))
            )?;
        }
    }
    if let Some(left) = left {
        // The divider lights up when the PANE has the keys. It is the border
        // between the two regions, so it is the honest place to say which side
        // you are on -- and it needs no extra row to do it.
        let colour = if pane_focused { 211 } else { 240 };
        for r in geo.header_h..rows.saturating_sub(geo.footer_h) {
            queue!(
                out,
                cursor::MoveTo(left, r),
                style::Print(fg(colour, '\u{2502}'))
            )?;
        }
    }
    Ok(())
}

/// `[ Repos ] · Orgs · Profile · Keys`
///
/// The active one is bracketed and bright; the rest are dim. Brackets rather
/// than a reverse-video block for the same reason the reticle uses them: this
/// repo's one visual idiom for "here" is a pair of brackets, and a second one
/// would be a second thing to learn.
///
/// Truncated from the RIGHT when it does not fit, so the leftmost tabs -- the
/// ones a screen puts first because they matter most -- survive a narrow
/// terminal.
fn tab_strip(tabs: &Tabs, cols: u16, focused: bool) -> String {
    let mut out = String::from(" ");
    let mut width = 1usize;
    if let Some(label) = &tabs.label {
        let label = crate::width::clean(label);
        out.push_str(&fg(245, &label));
        width += crate::width::width(&label);
    }
    for (i, name) in tabs.names.iter().enumerate() {
        let name = crate::width::clean(name);
        // Width is counted from the PLAIN text, because the SGR codes are
        // zero-width on screen and characters in the string -- the same
        // arithmetic `lib/sgr.sh` and the row renderer both have to do.
        let sep = if i == 0 && tabs.label.is_none() {
            ""
        } else {
            "  "
        };
        let plain = sep.len() + crate::width::width(&name) + if i == tabs.active { 4 } else { 0 };
        if width + plain + 1 > cols as usize {
            out.push_str(&fg(240, '\u{2026}'));
            break;
        }
        out.push_str(sep);
        if i == tabs.active {
            // The reticle's own pink when the strip HAS focus, dim when it
            // does not -- so "where am I" is answered by the same pair of
            // brackets everywhere in this program, rather than by a second
            // idiom to learn.
            let b = if focused { "211" } else { "240" };
            out.push_str(&format!(
                "\x1b[38;5;{b}m[\x1b[1;38;5;255m {name} \x1b[0m\x1b[38;5;{b}m]\x1b[0m"
            ));
        } else {
            out.push_str(&fg(244, name));
        }
        width += plain;
    }
    out
}

/// Where the list is looking: the scroll offset, the cursor, and the reticle
/// chasing it. Bundled because they always travel together and are always the
/// same three -- `draw_rows` took eight arguments once the header origin became
/// variable, which is a signal rather than a lint.
struct View<'a> {
    /// First column of this region.
    x0: u16,
    top: usize,
    sel: usize,
    /// `None` when another region has focus -- the list keeps its selection
    /// highlight, but the brackets belong to whoever the keys are talking to.
    reticle: Option<&'a Reticle>,
}

/// One row: reticle room, then marker, label, detail — and the badges pinned
/// to the right edge with everything before them truncated to fit.
///
/// Right-aligning is not cosmetic. Trailing the badge after a variable-length
/// family name put "GOOD" at a different column on every line and pushed the
/// longest ones straight across the divider into the pane.
fn draw_rows(
    out: &mut impl Write,
    rows: &[Row],
    vis: &[usize],
    view: &View,
    left: u16,
    y0: u16,
    height: usize,
) -> io::Result<()> {
    let View {
        x0,
        top,
        sel,
        reticle,
    } = *view;
    // `x0` is where this region starts and `left` where it ends, so one
    // function draws the narrow column, the full width, or the right-hand list
    // of a sidebar screen.
    let blank = " ".repeat(left.saturating_sub(x0) as usize);

    for i in 0..height {
        let y = y0 + i as u16;
        queue!(out, cursor::MoveTo(x0, y), style::Print(&blank))?;
        let Some(row) = vis.get(top + i).map(|&r| &rows[r]) else {
            continue;
        };
        if !row.selectable && row.label.is_empty() {
            continue;
        }

        // The label's column never depends on the reticle -- only the brackets
        // move. See the note in reticle.rs.
        //
        // Measured from `x0`, the region's left edge. Everything downstream --
        // the detail's column, the available width, the bracket positions --
        // derives from this, so the region offset lives in exactly one place.
        // It did not, for one commit, and the main list of a sidebar screen
        // drew over the sidebar.
        let indent = x0 + 2 + row.depth * 2 + SPREAD_ROOM;

        // Every marker plus one space of air between each and one before the
        // divider. Reserved FIRST, so the label is truncated to what is left
        // rather than pushing a badge across the divider.
        let badges: Vec<_> = row
            .badges
            .iter()
            .map(|(text, colour)| (crate::width::clean(text), colour))
            .collect();
        let badge_w: u16 = if badges.is_empty() {
            0
        } else {
            badges
                .iter()
                .map(|(t, _)| crate::width::width(t) as u16 + 1)
                .sum::<u16>()
                + 1
        };
        // With no badge, one cell is still kept back: it is where the closing
        // bracket goes on a label long enough to be truncated. Without it
        // such a row could never wear the brackets at all.
        let kept = badge_w.max(1);
        let avail = left.saturating_sub(indent + kept + 1);

        let marker = if row.group {
            if row.expanded {
                "\u{25be} "
            } else {
                "\u{25b8} "
            }
        } else {
            ""
        };
        let label = format!("{marker}{}", row.label);
        let label = truncate(&label, avail);
        let label_cols = crate::width::width(&label);
        // The reticle hugs the NAME, not the padding. A header padded for
        // column alignment would otherwise read `[ claude      ]`, with the
        // frame closing around whitespace.
        let name_cols = crate::width::width(label.trim_end());

        queue!(out, cursor::MoveTo(indent, y))?;
        let styled = if row.group {
            format!("\x1b[1m{label}\x1b[0m")
        } else if top + i == sel {
            format!("\x1b[1;38;5;255m{label}\x1b[0m")
        } else {
            fg(250, &label)
        };
        queue!(out, style::Print(styled))?;

        // The detail starts a full SPREAD_ROOM past the label, because that is
        // exactly where the reticle's closing bracket can reach at full
        // spread. Butting it up against the label instead meant the bracket
        // landed ON the first characters of the value — `[ nvim.editor ]etBrains…`.
        if let Some(detail) = &row.detail {
            let dx = indent + label_cols as u16 + SPREAD_ROOM;
            let room = left.saturating_sub(dx + badge_w + 1);
            if room > 2 {
                queue!(
                    out,
                    cursor::MoveTo(dx, y),
                    style::Print(fg(244, truncate(detail, room)))
                )?;
            }
        }

        // Packed against the right edge, but walked in REVERSE -- so the last
        // badge lands rightmost and a screen listing them in reading order gets
        // them in that order on screen. A table that pads each value to a fixed
        // width then comes out as columns.
        let mut bx = left;
        for (badge, color) in badges.iter().rev() {
            let w = crate::width::width(badge) as u16;
            bx = bx.saturating_sub(w + 1);
            queue!(out, cursor::MoveTo(bx, y), style::Print(fg(color, badge)))?;
        }

        if let Some(r) = reticle.filter(|r| r.row() == top + i && row.selectable) {
            let spread = r.spread();
            let open = indent.saturating_sub(spread + 1);
            // Held at the last free cell rather than dropped when the spread
            // would carry it past the edge or onto a badge.
            let close = (indent + name_cols as u16 + spread).min(left.saturating_sub(kept));
            if close > indent {
                queue!(
                    out,
                    cursor::MoveTo(open, y),
                    style::Print(fg(211, '[')),
                    cursor::MoveTo(close, y),
                    style::Print(fg(211, ']'))
                )?;
            }
        }
    }
    Ok(())
}

const SPREAD_ROOM: u16 = 5;

/// `text` in one of the 256 colours, reset after.
fn fg(colour: impl std::fmt::Display, text: impl std::fmt::Display) -> String {
    format!("\x1b[38;5;{colour}m{text}\x1b[0m")
}

/// Where the four regions sit, given what this screen asked for.
///
/// Both halves are variable now and both used to be assumed. A screen with a
/// `Tabs` strip needs TWO header rows (the strip and the rule under it) where a
/// plain title needs one; and a footer may be several lines, because a screen
/// with eight keys cannot name them on one 80-column row.
///
/// The footer one was a bug, not a feature request. `draw_footer` printed the
/// footer string at `rows - 1`, so a `\n` in it moved the cursor off the bottom
/// of the screen and SCROLLED the alternate screen by a line — every frame.
/// Absolute `MoveTo` on the next frame papered over most of it, which is why it
/// showed up as the pane's divider column appearing on the title row rather
/// than as anything that looked like scrolling.
///
/// A plain screen with a one-line footer gets exactly the geometry it had
/// before, which is what keeps `font` and `dots` pixel-identical.
struct Layout {
    header_h: u16,
    footer_h: u16,
    list_h: usize,
}

fn layout(screen_rows: u16, tabbed: bool, footer_lines: u16) -> Layout {
    let header_h = if tabbed { 2 } else { 1 };
    let footer_h = footer_lines.max(1);
    Layout {
        header_h,
        footer_h,
        // The spare row is the blank line between the last row and the footer,
        // which the original `rows - 3` also left.
        list_h: screen_rows.saturating_sub(header_h + footer_h + 1).max(1) as usize,
    }
}

/// Lines a footer string occupies.
fn footer_lines(footer: &str) -> u16 {
    footer.lines().count().max(1) as u16
}

/// How long to wait for the next keystroke. Three clocks, most urgent first.
///
/// `frame` covers both the reticle animating and work in flight: each wants to
/// be looked at again as soon as possible. A `Tick::Wake` is the slow clock —
/// honoured as asked, floored at one frame so a screen cannot ask for a busy
/// loop by requesting zero. With neither, the poll blocks for an hour, which is
/// what keeps an idle TUI off the CPU entirely.
fn poll_timeout(frame: bool, wake: Option<Duration>) -> Duration {
    if frame {
        FRAME
    } else if let Some(d) = wake {
        d.max(FRAME)
    } else {
        Duration::from_secs(3600)
    }
}

/// Where a screen opens, as a POSITION in the visible list: what it asked for,
/// clamped to what it drew.
///
/// The screen answers in its own row indices -- it knows nothing about a
/// filter -- so the answer is translated here. A row the filter has hidden
/// falls to the nearest visible one rather than nowhere.
///
/// Both guards are load-bearing. A screen computes its answer from its own row
/// walk, so an out-of-range one is a bug in that walk rather than a reason to
/// put the cursor past the end; and a spacer is a legal index that navigation
/// otherwise never lands on, so landing there once at startup would be the one
/// place a blank row can hold the cursor.
fn start_sel(screen: &dyn Screen, rows: &[Row], vis: &[usize]) -> usize {
    if vis.is_empty() {
        return 0;
    }
    let want_row = screen.initial_sel();
    let at = vis
        .iter()
        .position(|&r| r >= want_row)
        .unwrap_or(vis.len() - 1);
    first_selectable(rows, vis, at, 1)
}

/// The nearest selectable POSITION at or after `from`, searching in `dir`.
///
/// Falls back to `from` so a list that is entirely spacers -- or entirely
/// group headings, which a filter can produce -- cannot hang the caller.
fn first_selectable(rows: &[Row], vis: &[usize], from: usize, dir: isize) -> usize {
    scan(rows, vis, from as isize, dir).unwrap_or(from.min(vis.len().saturating_sub(1)))
}

/// Where a cursor at `sel` comes to rest: on the list, and on something
/// selectable -- the nearest above, or failing that the nearest below.
fn settled(rows: &[Row], vis: &[usize], sel: usize) -> usize {
    let at = sel.min(vis.len().saturating_sub(1)) as isize;
    scan(rows, vis, at, -1)
        .or_else(|| scan(rows, vis, at, 1))
        .unwrap_or(at as usize)
}

/// The first selectable position from `start`, walking in `dir`.
fn scan(rows: &[Row], vis: &[usize], start: isize, dir: isize) -> Option<usize> {
    let mut i = start;
    while i >= 0 && (i as usize) < vis.len() {
        if rows[vis[i as usize]].selectable {
            return Some(i as usize);
        }
        i += dir;
    }
    None
}

/// One step in `dir` through the VISIBLE list, skipping spacers, staying put
/// at the ends.
fn seek(rows: &[Row], vis: &[usize], from: usize, dir: isize) -> usize {
    scan(rows, vis, from as isize + dir, dir).unwrap_or(from)
}

/// Two regions with a rule between them: the picture on top, the numbers below.
///
/// The ORDER is the fix, not the arithmetic. An inline image is a negotiation —
/// the terminal scales it by rules this process cannot see, and the first
/// version's contact sheet came back more than twice as tall as the rows
/// reserved for it and painted straight over every metric. So the text region
/// is cleared AFTER the image is placed, which overwrites whatever it spilled,
/// and a divider says where one ends and the other begins.
fn draw_pane(
    out: &mut impl Write,
    pane: &Pane,
    x: u16,
    y: u16,
    width: u16,
    height: u16,
    // First text line to draw, for a pane with focus and more than fits.
    scroll: usize,
) -> io::Result<()> {
    let has_image = pane.image.as_ref().filter(|_| crate::image::supported());
    let text_h = (pane.lines.len() + 1).min(height as usize) as u16;
    // No picture, no picture region. Reserving it regardless pushed every
    // TEXT-ONLY pane to the bottom of the screen, under a rule that separated
    // nothing from nothing -- invisible while the only screen here had an
    // image to put up there, and wrong for every screen since.
    let img_h = match has_image {
        Some(_) => height.saturating_sub(text_h),
        None => 0,
    };

    if let Some(img) = has_image {
        if img_h >= 3 {
            queue!(out, cursor::MoveTo(x, y))?;
            out.write_all(&crate::image::inline(&img.png, img_h.min(img.rows)))?;
        }
    }

    // The rule is a SEPARATOR: it exists to divide the picture from the numbers
    // under it, so it appears only when there is a picture.
    let ruled = img_h >= 3;
    let text_y = if ruled { y + img_h + 1 } else { y + img_h };

    let blank = " ".repeat(width as usize);
    for r in text_y..(y + height) {
        queue!(out, cursor::MoveTo(x, r), style::Print(&blank))?;
    }
    if ruled {
        queue!(
            out,
            cursor::MoveTo(x, y + img_h),
            style::Print(fg(240, "\u{2500}".repeat(width as usize)))
        )?;
    }
    for (i, line) in pane.lines.iter().skip(scroll).enumerate() {
        let row = text_y + i as u16;
        if row >= y + height {
            break;
        }
        queue!(
            out,
            cursor::MoveTo(x, row),
            style::Print(truncate(line, width))
        )?;
    }
    Ok(())
}

/// The footer, one terminal row per line of it.
///
/// Each line is positioned with its own absolute `MoveTo` and the newlines are
/// never written. Printing the string as-is at `rows - 1` let a `\n` walk the
/// cursor off the bottom and scroll the whole alternate screen, once per frame.
fn draw_footer(
    out: &mut impl Write,
    footer: &str,
    cols: u16,
    rows: u16,
    depth: usize,
    footer_h: u16,
) -> io::Result<()> {
    // The screen says what its own keys do; the stack says what `q` does,
    // which the screen cannot know. It goes on the LAST line, so it is in the
    // same place whether the footer is one line or three.
    let back = if depth > 1 { "q back" } else { "q quit" };
    let lines: Vec<&str> = footer.lines().collect();
    let top = rows.saturating_sub(footer_h);
    for i in 0..footer_h {
        let y = top + i;
        let mut text = lines.get(i as usize).copied().unwrap_or("").to_string();
        if i + 1 == footer_h {
            if text.is_empty() {
                text = back.to_string();
            } else {
                text.push_str(" \u{b7} ");
                text.push_str(back);
            }
        }
        queue!(
            out,
            cursor::MoveTo(0, y),
            style::Print(" ".repeat(cols as usize)),
            cursor::MoveTo(1, y),
            style::Print(fg(244, truncate(&text, cols.saturating_sub(2))))
        )?;
    }
    Ok(())
}

/// Shorten to fit, measured in CELLS.
///
/// Delegates to `width::truncate`, which knows an emoji is two cells and an
/// ANSI escape is none. The character-counting version this replaces was the
/// reason emoji had to be kept out of every fixed-width column: one `char`
/// that draws two cells shifted everything after it by one.
///
/// And cleaned first. Every label, detail, pane line and footer a screen
/// supplies comes through here on its way to the terminal, which makes it the
/// place to take out anything that would drive it -- see `width::clean`.
fn truncate(s: &str, width: u16) -> String {
    crate::width::truncate(&crate::width::clean(s), width as usize)
}

#[cfg(test)]
mod tests {
    use super::{
        first_selectable, footer_lines, jump, layout, left_width, poll_timeout, scrolled, seek,
        settled, under_cursor, Duration, FRAME,
    };
    use crate::nav::Action;
    use crate::screen::Row;

    /// The difference between "no rows" and "no rows SHOWING". A screen with
    /// nothing in it has always been handed row 0 and copes; a screen whose
    /// rows are all filtered away must be handed nothing, or a key lands on a
    /// row that is not on screen.
    #[test]
    fn a_filter_hiding_everything_leaves_no_row_under_the_cursor() {
        let rows = list();
        assert_eq!(under_cursor(&rows, &[0, 2, 4], 1), Some(2));
        assert_eq!(under_cursor(&rows, &[], 0), None);
        assert_eq!(under_cursor(&[], &[], 0), Some(0));
    }

    /// One set of four jumps for the list and the sidebar alike, each landing
    /// on something selectable.
    #[test]
    fn the_four_jumps_land_on_selectable_rows() {
        let mut rows = list();
        rows.push(Row::spacer());
        let all: Vec<usize> = (0..rows.len()).collect();
        assert_eq!(jump(&rows, &all, 2, Action::Top, 2), 0);
        assert_eq!(jump(&rows, &all, 0, Action::Bottom, 2), 4);
        assert_eq!(jump(&rows, &all, 0, Action::PageDown, 2), 2);
        assert_eq!(jump(&rows, &all, 4, Action::PageUp, 2), 2);
        // Past either end is the end.
        assert_eq!(jump(&rows, &all, 4, Action::PageDown, 40), 4);
        assert_eq!(jump(&rows, &all, 2, Action::PageUp, 40), 0);
    }

    /// The offset only moves when the cursor would otherwise leave the rows
    /// drawn -- for the sidebar now, as well as for the list.
    #[test]
    fn scrolling_follows_the_cursor_and_no_further() {
        assert_eq!(scrolled(0, 3, 10), 0);
        assert_eq!(scrolled(0, 10, 10), 1);
        assert_eq!(scrolled(5, 2, 10), 2);
        assert_eq!(scrolled(5, 9, 0), 5);
    }

    /// A plain screen with a one-line footer must get the geometry it had
    /// before any of this was variable -- `font` and `dots` are that screen,
    /// and the numbers here are the ones the loop used to hard-code.
    #[test]
    fn a_plain_screen_keeps_the_layout_it_always_had() {
        let g = layout(48, false, 1);
        assert_eq!(g.header_h, 1);
        assert_eq!(g.footer_h, 1);
        assert_eq!(g.list_h, 45); // the old `rows - 3`
    }

    /// A strip costs one row, and only a screen that has one pays for it.
    #[test]
    fn a_tab_strip_costs_exactly_one_row() {
        assert_eq!(layout(48, true, 1).header_h, 2);
        assert_eq!(layout(48, true, 1).list_h, 44);
    }

    /// The bug this helper exists for. A multi-line footer used to be printed
    /// as one string at `rows - 1`, so its newlines scrolled the alternate
    /// screen once per frame; the visible symptom was the pane's divider
    /// column turning up on the title row. Now the rows are RESERVED.
    #[test]
    fn a_multi_line_footer_takes_rows_from_the_list_instead_of_scrolling() {
        assert_eq!(footer_lines("one line"), 1);
        assert_eq!(footer_lines("first\nsecond"), 2);
        // An empty footer is still one row, not zero.
        assert_eq!(footer_lines(""), 1);
        let g = layout(48, false, 2);
        assert_eq!(g.footer_h, 2);
        assert_eq!(g.list_h, 44);
        // Header, list, slack and footer account for every row.
        assert_eq!(g.header_h as usize + g.list_h + 1 + g.footer_h as usize, 48);
    }

    /// A terminal too short for the chrome must still leave a usable row
    /// rather than wrapping to zero and drawing nothing.
    #[test]
    fn a_tiny_terminal_still_gets_one_row() {
        assert_eq!(layout(3, true, 2).list_h, 1);
        assert_eq!(layout(1, true, 3).list_h, 1);
    }

    /// The three clocks, and which one outranks which. A screen polling a
    /// workflow run asks for seconds; the reticle asks for a frame; an idle
    /// screen asks for nothing and must not be woken for free.
    ///
    /// The third case is the one that matters: `Tick::Wake` is a floor on how
    /// long we may SLEEP, never a ceiling on how often we may draw, or a live
    /// run would make the reticle stutter every time it refreshed.
    #[test]
    fn a_slow_clock_does_not_become_a_frame_loop() {
        assert_eq!(poll_timeout(false, None), Duration::from_secs(3600));
        assert_eq!(
            poll_timeout(false, Some(Duration::from_secs(5))),
            Duration::from_secs(5)
        );
        assert_eq!(poll_timeout(true, Some(Duration::from_secs(5))), FRAME);
        // Asking for zero gets a frame, not a spin.
        assert_eq!(poll_timeout(false, Some(Duration::ZERO)), FRAME);
    }

    fn list() -> Vec<Row> {
        vec![
            Row::leaf("shell"),
            Row::spacer(),
            Row::leaf("nvim.ui"),
            Row::spacer(),
            Row::leaf("claude"),
        ]
    }

    /// The invariant spacers depend on. A cursor that can rest in a blank row
    /// turns a grouping device into a pothole: j would appear to do nothing.
    #[test]
    fn motion_never_lands_on_a_spacer() {
        let rows = list();
        let all: Vec<usize> = (0..rows.len()).collect();
        assert_eq!(seek(&rows, &all, 0, 1), 2);
        assert_eq!(seek(&rows, &all, 2, 1), 4);
        assert_eq!(seek(&rows, &all, 4, -1), 2);
        assert_eq!(seek(&rows, &all, 2, -1), 0);
    }

    /// A cursor is never left on a blank row, whoever moved the rows. Up to
    /// the nearest selectable one, which after a group closes is its heading;
    /// down only when there is nothing above.
    #[test]
    fn a_cursor_comes_to_rest_on_something_selectable() {
        let rows = list();
        let all: Vec<usize> = (0..rows.len()).collect();
        assert_eq!(settled(&rows, &all, 2), 2);
        assert_eq!(settled(&rows, &all, 1), 0);
        assert_eq!(settled(&rows, &all, 9), 4, "past the end is the end");
        let led = vec![Row::spacer(), Row::leaf("only")];
        assert_eq!(settled(&led, &[0, 1], 0), 1);
        assert_eq!(settled(&[], &[], 3), 0);
    }

    /// At the ends, staying put beats wrapping or landing on a blank.
    #[test]
    fn motion_stops_at_the_ends() {
        let rows = list();
        let all: Vec<usize> = (0..rows.len()).collect();
        assert_eq!(seek(&rows, &all, 0, -1), 0);
        assert_eq!(seek(&rows, &all, 4, 1), 4);
    }

    /// G aims at the last row, which is not necessarily a selectable one.
    #[test]
    fn ends_seek_backwards_to_something_selectable() {
        let mut rows = list();
        rows.push(Row::spacer());
        let all: Vec<usize> = (0..rows.len()).collect();
        assert_eq!(first_selectable(&rows, &all, rows.len() - 1, -1), 4);
        assert_eq!(first_selectable(&rows, &all, 1, 1), 2);
    }

    /// The left column has to leave a usable pane on a small terminal and stop
    /// growing on a large one.
    #[test]
    fn left_column_stays_sane() {
        assert!(left_width(80) <= 50);
        assert_eq!(left_width(200), 52);
        assert!(left_width(40) >= 24);
    }
}
