# CLAUDE.md — tui-core

The terminal UI framework under `font`, `dots` and `github`
([roest1/dotfiles](https://github.com/roest1/dotfiles)). Split out of `tui/reticle` on
2026-11-23 with its history. The entries below moved from dotfiles' `tui/CLAUDE.md` as they
were; trim them here.

## Layout

- `src/` — the framework. `term` (raw mode, alt screen, restore-on-panic),
  `nav` (the keymap), `reticle` (the animated brackets), `screen` (what a
  screen provides), `host` (a screen that is several screens — the tab strip's
  other half), `width` (how many CELLS a string occupies), `app` (the loop),
  `image` (inline images), `filter` (what `/` narrows a list to), `probe` (asks the
  terminal for its cell size and colors).
- `tests/pty.rs` — the event loop, driven through a real pty. The unit tests are of
  helpers; every bug the loop has had was in `app::event_loop`, which only this reaches.
  A change to the loop gets a check here. It waits for the screen to show something
  rather than sleeping, and a check that a key did NOT act uses a second key whose
  effect depends on the first. `cargo test --test pty -- <name>` runs one.

## Things not to undo

- **The pane clears its text region AFTER placing the image, not before.** An
  inline image is a negotiation, not an API: the terminal scales it by rules
  this process cannot see. Asked for a 147x14-cell sheet, wezterm bound on the
  WIDTH and returned it ~31 rows tall, straight over every metric below it. So
  the image is now sized by HEIGHT only, and the text region is blanked
  afterwards so anything it spilled gets overwritten. Both halves matter — the
  second is what makes a wrong guess survivable.
- **The two redraws are deliberate.** `app` redraws the row list per frame and
  the right-hand pane only when the *selection* changes, because the pane can
  hold an ~88KB PNG. Re-sending that at 60fps is ~5MB/s down a pty for an
  animation the image is not part of. See the header of `app.rs`.
- **The reticle must settle.** If `Reticle::moving()` never goes false, `app`
  polls on a 16ms timeout forever and the process spins at 60fps showing a
  static list. There is a test for exactly this.
- **`/` narrows the list, and no screen knows.** `app` filters the rows a
  screen returned and hands it `vis[sel]` — its OWN index — at the one boundary
  where the translation happens. So Actions, Branches, Orgs, Keys and the
  account screens all gained filtering in the commit that added it, without a
  line changing in any of them. Same trade as `Tick::Wake` and directional
  focus.

  **Substring, case-insensitive, ORDER-PRESERVING.** fzf did two things —
  matched loosely and reordered by score — and only the first is portable: a
  run list is newest-first because that is the question, and branches are
  grouped default-then-yours. A filter that reordered by score would answer
  "which matched best" over "what is going on here". Without ranking, a
  subsequence match is also worse than useless on short lists.

  Three things this had to get right, each found by using it:

  **A collapsed group survives any filter.** Its children are not in `rows` at
  all, so dropping it asserts there is no match inside when the filter cannot
  know. Collapsing a group while filtering emptied the screen with nothing on
  it to say why.

  **The cursor lands on a LEAF.** Landing on the heading above the match meant
  the next `enter` collapsed that group instead of opening what you searched
  for — which is how the blank screen above was reached.

  **`esc` clears the filter before it pops the screen**, and the footer says
  so. Otherwise a key pressed meaning "show me everything again" throws away
  the screen you were on.

  A filter in force is named in the footer when the field is closed. A list
  quietly hiding rows is the worst thing a filter can do.
- **`nav` has exactly two tables, and the mode lives there.** While a field has
  the keyboard a bare character is TEXT; otherwise it is `Action::Key(c)`.
  That is the whole reason filtering needed a framework change rather than a
  screen one — `o`, `r`, `b`, `e` would otherwise fire on the first letter of
  anything typed. `app` sets the mode from the frame on EVERY iteration rather
  than tracking it, so a push, pop, detach or tab switch cannot leave the
  keymap wrong; that failure would be silent and total.

  ctrl-c leaves from inside a field, because a field you cannot get out of is
  the one way this traps someone. ctrl-u clears it (PageUp outside). Arrows
  still navigate, so "type a bit, arrow down, enter" works.
- **`Flow::Ask` is screen-driven, and smaller than expected.** Filtering means
  the same thing everywhere so the framework does it; a VALUE only the screen
  knows what to do with comes back via `Screen::answered`, and the screen
  remembers what it asked — `app` carries the text and nothing else.

  One line, for IDENTIFIERS: a branch name, a secret's name, an environment.
  Anything longer already has a better answer, and finding that out is what
  made this small: **`gh pr create` and `gh pr edit` prompt for a title and
  body themselves**, on the real terminal, through `Flow::Detach`. The text
  widget was never the thing blocking PRs; filtering was.

  An empty or abandoned answer runs NOTHING. `--delete ""` is not something to
  find out about afterwards.
- **hjkl moves between REGIONS, not just within the list.** `Tab` used to be
  the only way to reach the strip, and a key whose whole job is changing what
  the other keys mean is a mode — the thing the nav contract exists to avoid.
  Riley's word for the fix was a smart TV, and it is the right model: the EDGE
  of a region is the door to the next one.

  `k` on the first row goes to the strip; `h`/`l` there change tab and `j`
  drops back in. `l` on a row with nothing to expand goes right into the pane,
  where `j`/`k` scroll it and `h` comes back.

  **The rule that keeps the screens out of it: a direction acts within the
  region, and when it cannot, focus moves instead.** For `h`/`l` that is read
  straight off `Flow::Continue`, which already meant "nothing happened" — no
  screen had to change to take part.

  Two things this forced, both of which were wrong before and invisible:

  **`Action::Open` is not a synonym for `Activate`.** The github screens mapped
  `Activate | Open` together, so `l` on a run drilled into its jobs and the
  pane beside it could never be reached. `l` now only ever expands or moves
  right; `enter` does the thing. `Menu` takes `Activate` alone, because a
  second key that runs a delete is the one place an extra way to act is a cost.

  **Focus enters the pane whenever one EXISTS**, not only when it overflows.
  Gating on scrollability was the first attempt and it reads as a dead key: on
  a run whose body happens to fit, `l` silently did nothing — exactly the "why
  won't it move" the model is meant to remove. The divider lights up and the
  footer changes, so the move is visible either way.

  Only the focused region wears the reticle brackets, and each region states
  its OWN keys in the footer — the screen speaks for the list alone, since a
  footer still advertising `enter runs it` while the keys were talking to the
  pane is the kind of small lie that gets believed.
- **`tui_core::width` measures CELLS, and the emoji ban it replaced was a bug
  fix pointed the wrong way.** Emoji were kept out of every fixed-width column
  because `✅` is one `char` and TWO cells, so a column of them drifted. That
  was arithmetic, not a reason to lose the one thing in a dense list you can
  find without reading — so the layout counts cells now and the glyphs are
  back: `✅ ok` / `❌ fail` badges, a 🔒 on a protected branch, and a glyph on
  every tab.

  `width()` skips ANSI escapes (characters in the string, nothing on screen),
  returns 0 for combining marks and variation selectors, and 2 for the
  emoji-presentation ranges. The ranges are written out rather than
  approximated because `↑` (2191) and `⭐` (2B50) are NEIGHBOURS of each other's
  blocks and are one and two cells respectively — an approximation that swept
  up a whole block would break the branch table's arrows. **Measured, not
  assumed: `⏭` is NARROW** (so the skip badge uses `⏩`), which is why the bash
  version wrote `"⏭ "` with a trailing space.

  **This confusion has now bitten three times in one session** — twice in a
  test and once in the pty harness — always the same way: `str::find` returns
  BYTES, `chars().count()` returns CHARACTERS, and neither is what the terminal
  does. If you are asserting alignment, assert `width::width`.
- **`Tab` is bound to nothing.** It moved the tabs for one commit, and that
  was wrong for the reason the whole directional model exists: a key whose only
  job is reaching a region is a mode by another name. Keeping it as a shortcut
  sounds harmless and is not — it is a second way to express one thing, so it
  is a second thing to learn, document and keep working. `k` off the top row
  reaches the strip and `h`/`l` move along it. The `Host` footer says so, since
  there is no key to discover.
- **`Split::Sidebar` is TWO row regions, and `hjkl` crosses them.** Filters
  left, list right — the inverse of `ListPane`, and the shape of the Issues and
  PR mocks. `Screen::sidebar()` and `Screen::on_sidebar()` are the second pair;
  split in two rather than passed a region so a screen cannot read an index
  against the wrong list, which is the one mistake here that would act on a row
  nobody was looking at.

  **`l` opens what it can and crosses when it cannot**, which is the same edge
  rule as everywhere else and is load-bearing here: every row in the PR sidebar
  is a dropdown, so without the second half `l` toggled dropdowns forever and
  the list was unreachable. An open control has nothing left to open, so `l`
  moves on.

  `draw_rows` measures `indent` from the region's `x0`. It did not for one
  commit and the main list drew straight over the sidebar.
- **Forward EVERYTHING from `Host` that a screen says about itself.** This has
  now been got wrong THREE times in the same way — `split()`, then
  `sidebar()`, then `answered()` — and none of them looked like a forwarding
  bug from the symptom: a table with no names in it, an empty left column, and
  a question that opened a field, took the text, and did nothing with it.

  The rule is now stated on the trait itself, in `screen.rs`: **a method with a
  default body is a method `Host` has to forward.** The default is exactly what
  makes these quiet — a host answering for it compiles and runs. The required
  methods are safe, because a host missing one would not build.
  `host::tests::everything_comes_from_the_active_tab` covers the whole set
  rather than the method that broke, and adding a defaulted method to `Screen`
  means adding a line to it in the same change.

  The third one took a while to see because `Menu` had been the only thing that
  asked, and a `Menu` is PUSHED rather than hosted — so `Flow::Ask` worked
  everywhere it had been used, right up to the first hosted screen that asked
  for itself.
- **`Screen::split()` picks the layout, per screen.** `ListPane` (the default),
  `Full`, or `Sidebar`. The shapes genuinely differ: a workflow run is a short list with a
  long story beside it, where a branch list is a TABLE — seven facts per row
  and nothing a pane could add. A `Full` screen is never asked for a pane at
  all, so it need not implement one usefully.

  **`Host` must FORWARD it**, because the layout belongs to the tab rather than
  the strip above it. Forgetting that made `Split::Full` silently do nothing:
  the host answered for the default and the table's badges, packed into a
  52-column sidebar, truncated every branch name to nothing. There is a test.
- **`Row::badges` is a VEC, and it does not self-align.** One badge could only
  hold the last of a table's seven columns. `app` packs them against the right
  edge, rightmost last so a screen listing them in reading order gets that
  order on screen — but it does not align them. A screen that wants COLUMNS
  pads each value to a fixed width itself, because it knows its own data and
  `rows()` deliberately does not know the terminal width.
- **Tabs are for siblings; the stack is for drilling in.** `tui_core::Host` owns
  one `Screen` per tab and forwards to the active one, so a screen does not
  know it became a tab — `Actions` was written before tabs existed and is a tab
  now, unchanged. `h`/`l` on the strip move along; `Flow::Push` from inside a
  tab opens over the WHOLE host and `q` returns to the tab you left.

  Six things about one repo are not six levels deep. A stack would make Issues
  → PRs a pop and a push through a hub that exists only to be passed through.

  **A one-tab strip is a title that cost a row**, so a level gets a `Host` only
  once it has two real screens. The user level has four and an org has three;
  the REPO level has one ported of six designed, so enter on a repo still
  pushes `Actions` directly. It becomes a `Host` in the commit that adds the
  second repo screen, and nothing else changes.
- **`Host` remembers each tab's cursor, and `Flow::Retarget` is how.** A tab
  switch replaces every row, so `app` cannot carry the selection across —
  clamping a 40-row position into a 3-row tab lands somewhere arbitrary.
  `Retarget` makes `app` re-ask `initial_sel`, which the host answers with the
  mark it saved on the way out. `app` never learns what a tab is.
- **The pane does not reserve an image region it has no image for.** It used
  to, unconditionally, which pushed every TEXT-ONLY pane to the bottom of the
  screen under a rule that separated nothing from nothing. Invisible while the
  only screen here was `font`'s specimen; wrong for `dots`, wrong for all of
  `github`. The rule is a SEPARATOR and appears only when there is something to
  separate. The image path's arithmetic is unchanged.
- **`Screen::initial_sel` is asked of the screen, not passed to it.** `github`
  inside a repo opens on that repo's row, and which row that is depends on
  which lanes the screen decided to open — a caller would have to reimplement
  the row walk to know. `app` clamps the answer to the rows actually drawn and
  skips a spacer, so a stale one cannot park the cursor in a blank row, which
  is the one place navigation otherwise never lands.

  **`app` asks PUSHED screens too, and that was a bug for one commit.** The
  push path hard-coded `sel: 0`, which was invisible for as long as nothing
  overrode `initial_sel` and then silently wrong for the first screen that did:
  drilling into a failed run opened on the run's first job instead of on the
  step that failed. Both paths go through `start_sel` now.
- **`Tick::Wake(Duration)` is the third clock, and it exists because `Busy` is
  a FRAME clock.** `Busy` means "look again next frame", which is right for
  work that lands in milliseconds — the font browser rasterising a specimen —
  and wrong for a network round trip. A nine-minute workflow run reported as
  `Busy` would spin the process at 60fps for nine minutes to notice an event
  that can only arrive on a five-second boundary: the laptop fan `app`'s
  blocking poll exists to avoid, reintroduced by the one screen that most needs
  to stay quiet while it waits.

  So the SCREEN picks the clock, because the screen is what knows which kind of
  work it started. `Wake` is a floor on how long `app` may sleep, never a
  ceiling on how often it may draw — an animating reticle still outranks it, or
  a refresh would make the brackets stutter. `app::tests::a_slow_clock_does_not_become_a_frame_loop`
  pins all three orderings, and the measured result is the point: the Actions
  screen emits **zero bytes** over twenty idle seconds on a repo whose runs have
  all finished.
- **The pane DOES NOT SCROLL, and a log is the one thing whose end matters.**
  `draw_pane` cuts whatever does not fit and says nothing, so a four-hundred
  line body in a forty-row pane loses its own tail — which for a failed run is
  the summary and, worse, the error. This is easy to forget because every
  previous log view in this repo could scroll: fzf's preview took the mouse and
  a pager scrolls by definition, so "print it all" was always available before.
  It is not here.

  So `render::tail_budget` divides the room among the failed steps and each gets
  a TAIL, with a marker naming how much it hid and which key pages the rest.
  What survives a pane too small for everything is the STRUCTURE — the step
  names and the summary — because those answer "is it green" and "which step",
  and a tail of four lines answers neither.
  `render::tests::a_body_never_outgrows_the_pane_it_was_given` asserts the fit
  at five pane heights; the first draft showed the runner installing chromium
  and hid the assertion that failed.
- **Do not add keys to `nav` for one screen.** `Action::Key(char)` is the escape hatch, and
  it exists so the shared table stays shared.
- **Every `Flow` goes through `app::apply`.** There were four copies of that match, one
  per handler, and they drifted: a sidebar pick returned `Flow::Dirty` and the list beside
  it was never re-read, so `enter` opened a row other than the one drawn. A new place that
  asks a screen something calls `apply` with the answer.
- **The row under the cursor is an `Option`.** A filter that hides every row leaves
  `None`, and nothing row-addressed is sent to the screen. Falling back to row 0 let a key
  act on a row that was not on screen.
- **The mouse is not captured.** Nothing reads it. Capturing it woke the loop on every
  pointer movement (about 2KB redrawn each), blocked text selection and left the wheel
  dead; uncaptured, the terminal sends the wheel as arrow keys.
- **Pastes are bracketed, because the mouse is not captured.** A middle click pastes now.
  Unbracketed, each pasted letter is a single-key action; bracketed, the paste is one
  event, typed into an open field (first line only) and dropped everywhere else.
- **A frame is assembled in a `Vec` and written once.** Not a `BufWriter`: a panic restores
  the terminal and then unwinds, and a `BufWriter` dropped on the way out flushes its
  half-drawn frame over the panic message.
- **Only time spent animating steps the reticle.** Counting the wait for a key spent four
  frames of travel at once, so a single keypress never showed the spread opening.
- **The probe's deadline is a second, and DA1 ends it early.** A reply that arrives after
  the deadline is read by the event loop as keystrokes.
- **A tab that answers `Flow::Retarget` gets its mark dropped.** `Host::initial_sel`
  prefers the cursor it remembered, so a hosted screen that asked to move its own
  cursor (a group it closed, a row inserted above) was handed the old position
  straight back. The same screen pushed instead of hosted worked.
- **The cursor never rests on an unselectable row.** `app` settles it at the top of
  every frame: up to the nearest selectable row, down only if there is none above.
  A screen that closed a group from a child and answered `Flow::Dirty` used to leave
  it on the blank row underneath: no brackets, an empty pane, a dead `enter`.
- **What a screen draws cannot drive the terminal.** Every title, label, detail, badge,
  pane line and footer goes through `width::clean` on its way out: colours survive
  (`ESC [ … m` with only digits, `;` and `:`), every other escape sequence and every
  control character is dropped. A screen draws text it was handed (a pull request's
  title, a line of a CI log), and `ESC ] 52` in that text set the clipboard. It is an
  allowlist in the draw path so that no screen has to remember, and
  `a_screen_cannot_drive_the_terminal` holds it.
- **`Flow::DetachIn` exists so nothing has to go through a shell to change directory.**
  `gh pr checkout` acts on the repository it is standing in and has no flag to be told
  otherwise. The alternative was `sh -c 'cd … && exec …'`, which is the shell `Detach`
  promises nothing goes through, and it is what the confirm pane then showed.
- **`initial_side_sel` is asked wherever `initial_sel` is.** A sidebar is a column of
  dropdowns; closing one from an option inside it moves every row below, and
  `Flow::Retarget` could only say where the MAIN cursor belongs.
- **A detached child owns ctrl-c.** `detach` holds a do-nothing SIGINT handler while the
  child runs, or stopping the child kills the TUI with it. A handler, not `SIG_IGN`,
  which the child would inherit.

## Sharpness, and where it comes from

An inline image is scaled by the terminal to fit the cells it is given, so a
canvas that is not already the right number of PIXELS gets resampled — and
resampled text is blur. `tui_core::probe` asks the terminal for its cell size
(`CSI 16 t`) at startup and the canvases are built at exactly
`cols x cell_w` by `rows x cell_h`, so the image is drawn 1:1.

Nothing about the rasteriser was ever the problem. Verified by rendering
against a terminal that reports a 21px cell and checking the canvas comes back
at 21.00px per row.
## Left-column layout

Four slots, and three of them exist because the first version got it wrong:

- **label** — bold when the row is a `group`, so headers read as headers with
  no glyph needed to say so.
- **detail** — dim, and it starts a full `SPREAD_ROOM` past the label. That gap
  is not padding: it is exactly where the reticle's closing bracket reaches at
  full spread, and without it the bracket landed on the value —
  `[ nvim.editor ]etBrainsMono…`.
- **badge** — RIGHT-ALIGNED to the pane edge, with everything before it
  truncated to fit. Trailing it after a variable-length name put `GOOD` at a
  different column on every line and pushed the longest ones across the
  divider into the pane.
- **spacers** — `Row::spacer()`, skipped by navigation. A cursor that can rest
  in a blank row turns a grouping device into a pothole.

The reticle brackets the NAME, not the padded label, so a header padded for
column alignment does not render as `[ claude      ]`.
