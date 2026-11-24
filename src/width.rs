//! How many terminal cells a string actually occupies.
//!
//! ## Why this exists rather than `.chars().count()`
//!
//! A terminal is a fixed grid and most characters take one cell, so counting
//! characters is right until it is not. Emoji are East Asian **Wide**: `✅` is
//! one `char` and TWO cells. Every piece of layout here -- truncating a label
//! to fit, reserving room for a badge, deciding whether the tab strip has run
//! out of width -- was counting characters, so an emoji anywhere in a
//! fixed-width column silently shifted everything after it by one cell per
//! emoji.
//!
//! The first answer to that was to ban emoji from columns. That was the wrong
//! trade: the glyphs carry colour and meaning that a word does not, which is
//! precisely why the fzf implementation used them. The right answer is to
//! measure properly, which is this file.
//!
//! ## Why not the `unicode-width` crate
//!
//! It is the correct general answer and it is a dependency plus a compile for
//! a complete table of a problem this crate has a bounded version of: the only
//! wide characters that reach these screens are the ones chosen in
//! `github::paint` and `github::model`. So the ranges below cover Unicode's
//! emoji-presentation blocks -- the set that is wide by default, with no
//! variation selector needed -- and everything else is one cell.
//!
//! If a screen ever needs CJK text, swap this for the crate. The signature is
//! the same and the call sites would not change.

/// `s` with everything that could DRIVE the terminal taken out.
///
/// A screen draws text it was handed -- a branch name, a pull request's title,
/// a line of a CI log -- and whoever wrote that text chose its bytes. A
/// terminal obeys what it is sent: `ESC ] 52` sets the clipboard, `ESC [ 2 J`
/// clears the screen, a carriage return moves the cursor into the list beside
/// the pane. So `app` passes everything a screen gives it through here on the
/// way out, and a screen cannot forget to.
///
/// Kept: colours, which screens use on purpose -- `ESC [ ... m` with nothing
/// in it but digits, `;` and `:`. Dropped: every other escape sequence, whole,
/// and every control character. A tab becomes a space, since the terminal
/// would otherwise jump to a tab stop the layout knows nothing about.
///
/// An allowlist, not a list of what is dangerous. The second would need to
/// be complete, about every terminal.
pub fn clean(s: &str) -> std::borrow::Cow<'_, str> {
    if !s.chars().any(char::is_control) {
        return s.into();
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.next() {
                // CSI: parameters, then one final byte that says what it is.
                Some('[') => {
                    let mut params = String::new();
                    let last = chars.by_ref().find(|&p| {
                        let done = ('\x40'..='\x7e').contains(&p);
                        if !done {
                            params.push(p);
                        }
                        done
                    });
                    let colour = params
                        .chars()
                        .all(|p| p.is_ascii_digit() || p == ';' || p == ':');
                    if last == Some('m') && colour {
                        out.push_str("\x1b[");
                        out.push_str(&params);
                        out.push('m');
                    }
                }
                // The ones that carry a string: OSC, DCS, SOS, PM, APC. Gone
                // up to the bell or the `ESC \` that ends them -- or to the
                // end, if nothing does.
                Some(']' | 'P' | 'X' | '^' | '_') => {
                    while let Some(p) = chars.next() {
                        if p == '\x07' || (p == '\x1b' && chars.next_if_eq(&'\\').is_some()) {
                            break;
                        }
                    }
                }
                // Any other escape is itself and one character.
                _ => {}
            },
            '\t' => out.push(' '),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out.into()
}

/// Cells one character occupies.
///
/// Zero for a combining mark, two for a default-emoji-presentation character,
/// one for everything else.
pub fn char_width(c: char) -> usize {
    let u = c as u32;
    match u {
        // Combining marks: drawn on top of the previous cell, so they add
        // nothing. A variation selector (FE0F) is in here, which matters --
        // it is what turns `⚠` into an emoji presentation without widening it
        // in most terminals.
        0x0300..=0x036F | 0xFE00..=0xFE0F | 0x200D => 0,
        // The ranges Unicode marks Emoji_Presentation=Yes below U+1F000.
        // Written out rather than approximated, because `↑` (2191) and `★`
        // (2605) are NEIGHBOURS of wide characters and are one cell -- an
        // approximation that swept up the whole block would break the branch
        // table's arrows.
        0x231A..=0x231B
        | 0x23E9..=0x23EC
        | 0x23F0
        | 0x23F3
        | 0x25FD..=0x25FE
        | 0x2614..=0x2615
        | 0x2648..=0x2653
        | 0x267F
        | 0x2693
        | 0x26A1
        | 0x26AA..=0x26AB
        | 0x26BD..=0x26BE
        | 0x26C4..=0x26C5
        | 0x26CE
        | 0x26D4
        | 0x26EA
        | 0x26F2..=0x26F3
        | 0x26F5
        | 0x26FA
        | 0x26FD
        | 0x2705
        | 0x270A..=0x270B
        | 0x2728
        | 0x274C
        | 0x274E
        | 0x2753..=0x2755
        | 0x2757
        | 0x2795..=0x2797
        | 0x27B0
        | 0x27BF
        | 0x2B1B..=0x2B1C
        | 0x2B50
        | 0x2B55 => 2,
        // Everything in the supplemental symbol planes: the pictographs,
        // transport, flags and the rest. Wide by default, all of them.
        0x1F004 | 0x1F0CF | 0x1F18E | 0x1F191..=0x1F19A => 2,
        0x1F200..=0x1F320
        | 0x1F32D..=0x1F335
        | 0x1F337..=0x1F37C
        | 0x1F37E..=0x1F393
        | 0x1F3A0..=0x1F3CA
        | 0x1F3CF..=0x1F3D3
        | 0x1F3E0..=0x1F3F0
        | 0x1F3F4
        | 0x1F3F8..=0x1F43E
        | 0x1F440
        | 0x1F442..=0x1F4FC
        | 0x1F4FF..=0x1F53D
        | 0x1F54B..=0x1F54E
        | 0x1F550..=0x1F567
        | 0x1F57A
        | 0x1F595..=0x1F596
        | 0x1F5A4
        | 0x1F5FB..=0x1F64F
        | 0x1F680..=0x1F6C5
        | 0x1F6CC
        | 0x1F6D0..=0x1F6D2
        | 0x1F6D5..=0x1F6D7
        | 0x1F6EB..=0x1F6EC
        | 0x1F6F4..=0x1F6FC
        | 0x1F7E0..=0x1F7EB
        | 0x1F90C..=0x1F93A
        | 0x1F93C..=0x1F945
        | 0x1F947..=0x1F9FF
        | 0x1FA70..=0x1FAFF => 2,
        _ => 1,
    }
}

/// Cells a whole string occupies, ANSI escapes excluded.
///
/// The escapes have to be skipped rather than counted: they are characters in
/// the string and nothing on screen, which is the same arithmetic `lib/sgr.sh`
/// and `make help`'s typewriter both have to do.
pub fn width(s: &str) -> usize {
    let mut total = 0;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            rest_of_escape(&mut chars, |_| {});
            continue;
        }
        total += char_width(c);
    }
    total
}

/// Consume the escape whose `ESC` was just read, handing each character to
/// `each`. CSI ... final-byte: everything up to and including the first letter
/// belongs to it.
fn rest_of_escape(chars: &mut std::str::Chars, mut each: impl FnMut(char)) {
    for e in chars.by_ref() {
        each(e);
        if e.is_ascii_alphabetic() {
            break;
        }
    }
}

/// Pad `s` on the right to `cells`, measured properly. Longer strings are
/// returned unchanged -- the caller truncates if it needs to.
pub fn pad(s: &str, cells: usize) -> String {
    format!("{s}{}", " ".repeat(cells.saturating_sub(width(s))))
}

/// Pad `s` on the LEFT to `cells`, for a right-aligned column.
pub fn rpad(s: &str, cells: usize) -> String {
    format!("{}{s}", " ".repeat(cells.saturating_sub(width(s))))
}

/// Shorten `s` to at most `cells`, adding an ellipsis when it had to cut.
///
/// Cuts on a character boundary and never inside an escape, so a truncated
/// string cannot leave the terminal in whatever state a severed SGR implied.
/// And it ends with a reset if it carried any styling, because the cut is
/// what dropped the reset the string ended with: a truncated red line in the
/// pane used to turn the plain lines under it red.
pub fn truncate(s: &str, cells: usize) -> String {
    if width(s) <= cells {
        return s.to_string();
    }
    if cells == 0 {
        return String::new();
    }
    let budget = cells - 1;
    let mut out = String::new();
    let mut used = 0;
    let mut styled = false;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            styled = true;
            out.push(c);
            rest_of_escape(&mut chars, |e| out.push(e));
            continue;
        }
        let w = char_width(c);
        if used + w > budget {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('\u{2026}');
    if styled {
        out.push_str("\x1b[0m");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The characters this repo's screens actually draw, and the pairs that
    /// make the naive answer wrong.
    #[test]
    fn the_glyphs_these_screens_use_measure_correctly() {
        // One cell: the markers and arrows the branch table lines up with.
        for c in [
            'a', ' ', '\u{25cf}', '\u{2605}', '\u{2191}', '\u{2193}', '\u{2713}', '\u{2502}',
        ] {
            assert_eq!(char_width(c), 1, "{c:?}");
        }
        // Two cells: the status emoji, which is the whole reason this exists.
        for c in [
            '\u{2705}',
            '\u{274c}',
            '\u{23f3}',
            '\u{1f512}',
            '\u{1f680}',
            '\u{1f33f}',
        ] {
            assert_eq!(char_width(c), 2, "{c:?}");
        }
    }

    /// `★` and `✅` are both one `char`. Counting characters says they are the
    /// same width; they are not, and that one cell per emoji is what shifted
    /// every column after it.
    #[test]
    fn counting_characters_is_what_this_replaces() {
        assert_eq!("\u{2605}".chars().count(), "\u{2705}".chars().count());
        assert_ne!(width("\u{2605}"), width("\u{2705}"));
        assert_eq!(width("\u{2605}"), 1);
        assert_eq!(width("\u{2705}"), 2);
    }

    /// Escapes are characters in the string and nothing on screen.
    #[test]
    fn ansi_escapes_occupy_no_cells() {
        assert_eq!(width("\x1b[38;5;203mfail\x1b[0m"), 4);
        assert_eq!(width("\x1b[1m\u{2705} ok\x1b[0m"), 5);
    }

    /// A variation selector styles the previous character without widening it.
    #[test]
    fn a_combining_mark_adds_nothing() {
        assert_eq!(width("\u{26a0}\u{fe0f}"), 1);
    }

    /// Padding a column of mixed glyph widths has to land on one boundary, or
    /// the table it is building is not a table.
    #[test]
    fn padding_lines_up_across_mixed_widths() {
        let col = [pad("\u{2705} ok", 8), pad("fail", 8), pad("\u{23f3}", 8)];
        for c in &col {
            assert_eq!(width(c), 8, "{c:?}");
        }
        let right = [rpad("8h", 5), rpad("\u{2705}", 5)];
        for c in &right {
            assert_eq!(width(c), 5, "{c:?}");
        }
    }

    /// What a screen draws must not be able to drive the terminal. These are
    /// the things a pull request's title or a line of a log could carry.
    #[test]
    fn only_colours_get_through() {
        // The ordinary case costs nothing and changes nothing.
        assert!(matches!(
            clean("feat/x \u{2705}"),
            std::borrow::Cow::Borrowed(_)
        ));
        // Colours are kept exactly.
        let red = "\x1b[1;38;5;203mfail\x1b[0m";
        assert_eq!(clean(red), red);
        assert_eq!(clean("\x1b[38:2::1:2:3mx"), "\x1b[38:2::1:2:3mx");
        // The clipboard, by either terminator, and an unterminated one.
        assert_eq!(clean("a\x1b]52;c;aGk=\x07b"), "ab");
        assert_eq!(clean("a\x1b]52;c;aGk=\x1b\\b"), "ab");
        assert_eq!(clean("a\x1b]52;c;aGk="), "a");
        // Clearing the screen, moving the cursor, asking the terminal to
        // report -- and a private sequence that merely ENDS in `m`.
        assert_eq!(clean("a\x1b[2Jb\x1b[10;10Hc\x1b[5nd"), "abcd");
        assert_eq!(clean("a\x1b[?1049mb\x1b[>4;2mc"), "abc");
        // A device control string, and a two-character escape.
        assert_eq!(clean("a\x1bPq#0;2;0;0;0\x1b\\b\x1bcc"), "abc");
        // Bare control characters. A tab is a space; a bell is nothing.
        assert_eq!(clean("a\rb\nc\x07d\te\u{9b}2Jf\x7f"), "abcd e2Jf");
        // An escape cut off by the end of the string takes nothing with it
        // that was not part of it.
        assert_eq!(clean("ab\x1b"), "ab");
        assert_eq!(clean("ab\x1b[31"), "ab");
    }

    /// Truncation must respect cells too, and must never sever an escape --
    /// a half-written SGR leaves the terminal in whatever state the fragment
    /// implied, for everything printed after it.
    #[test]
    fn truncation_counts_cells_and_keeps_escapes_whole() {
        assert_eq!(truncate("abcdef", 4), "abc\u{2026}");
        assert_eq!(width(&truncate("abcdef", 4)), 4);
        // Two emoji is four cells, so a 3-cell budget keeps one and ellipses.
        assert_eq!(width(&truncate("\u{2705}\u{274c}", 3)), 3);
        let styled = truncate("\x1b[31mabcdef\x1b[0m", 4);
        assert!(styled.contains("\x1b[31m"), "{styled:?}");
        assert!(!styled.ends_with('\x1b'));
        // The cut dropped the string's own reset, so one is put back: whatever
        // is printed next must not inherit the red.
        assert_eq!(styled, "\x1b[31mabc\u{2026}\x1b[0m");
        assert_eq!(width(&styled), 4);
        // Nothing to do when it already fits.
        assert_eq!(truncate("ab", 8), "ab");
    }
}
