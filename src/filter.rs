//! Type to narrow the list.
//!
//! ## What is ported from fzf, and what deliberately is not
//!
//! fzf gave fuzzy-filter-as-you-type for free, and it did two things: it
//! MATCHED loosely, and it REORDERED by score. The matching is what people mean
//! by "type to filter". The reordering is not portable here, and dropping it is
//! the one real difference:
//!
//! these lists have meaningful order. Workflow runs are newest-first because
//! that is the question being asked; branches are grouped default-then-yours;
//! an org's repos are sorted by last push. A filter that reordered by match
//! score would answer "which of these matched best" over "what is going on
//! here", and the second is why the screen exists. So rows keep their order and
//! the non-matching ones simply go away.
//!
//! Matching is therefore case-insensitive SUBSTRING rather than subsequence.
//! Without ranking, a subsequence match is worse than useless on short lists --
//! `rb` matching `README` via r…b is noise you cannot sort out of the way.
//!
//! ## Why it lives in the framework
//!
//! Because then every screen already has it. `app` filters the rows a screen
//! returned; the screen is never told. That is the same trade `Tick::Wake` and
//! directional focus made, and it is why Actions, Branches, Orgs and the
//! account screens gained filtering in the commit that added this without one
//! line changing in any of them.
//!
//! ## Trees
//!
//! A matching leaf whose group header does not match would otherwise float
//! free, under whatever heading happened to survive above it. So a group is
//! kept when anything beneath it matches, which is computed from `depth`: a row
//! at depth `d` owns the following rows of greater depth, until the next row at
//! `d` or less.
//!
//! Spacers drop out entirely while a filter is active. They exist to group
//! things visually, and three blank lines between two surviving rows is not
//! grouping.

use crate::screen::Row;

/// The row to put the cursor on once a filter is applied: the first LEAF, or
/// the first anything if there are no leaves.
///
/// Preferring a leaf is the difference between the filter finding something and
/// the filter looking like it found something. Landing on the group heading
/// above the match meant the next `enter` collapsed that group rather than
/// opening what had just been searched for.
pub fn first_leaf(rows: &[Row], vis: &[usize]) -> usize {
    vis.iter()
        .position(|&r| rows[r].selectable && !rows[r].group)
        .or_else(|| vis.iter().position(|&r| rows[r].selectable))
        .unwrap_or(0)
}

/// Does this row match? Label and detail together, since a PR's author and a
/// branch's subject are both things you would type.
pub fn matches(row: &Row, needle: &str) -> bool {
    matches_folded(row, &fold(needle))
}

/// Lowercased a character at a time, so both sides of the comparison fold the
/// same way whatever surrounds a letter.
fn fold(s: &str) -> String {
    // The same answer for ASCII, which is nearly every row, at a quarter of
    // the cost -- and this runs for every row on every frame a filter is up.
    if s.is_ascii() {
        return s.to_ascii_lowercase();
    }
    s.chars().flat_map(char::to_lowercase).collect()
}

/// The rows a group owns: those after it at a greater depth, up to the next
/// row at its own depth or less.
fn owned(rows: &[Row], i: usize) -> impl Iterator<Item = usize> + '_ {
    (i + 1..rows.len()).take_while(move |&j| rows[j].depth > rows[i].depth)
}

/// `matches`, for a needle that is ALREADY folded -- so a whole list is
/// compared against one folding of what was typed rather than one per row.
fn matches_folded(row: &Row, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let detail = row.detail.as_deref().unwrap_or_default();
    fold(&format!("{} {detail}", row.label)).contains(needle)
}

/// Indices of the rows to draw, in the order the screen gave them.
///
/// With an empty filter this is every row, which is what keeps the unfiltered
/// path identical to not having a filter at all.
pub fn visible(rows: &[Row], needle: &str) -> Vec<usize> {
    if needle.is_empty() {
        return (0..rows.len()).collect();
    }
    let needle = fold(needle);
    let mut keep: Vec<bool> = rows
        .iter()
        .map(|r| r.selectable && matches_folded(r, &needle))
        .collect();
    // A group whose own NAME matched brings everything under it, because that
    // is what typing the name of a group means. Without this the heading came
    // back alone, open and empty, and `enter` on it closed it.
    for i in 0..rows.len() {
        if rows[i].group && keep[i] {
            for j in owned(rows, i) {
                keep[j] = true;
            }
        }
    }
    // A COLLAPSED group always survives, whatever the needle. Its children are
    // not in `rows` at all -- the screen never emitted them -- so the filter
    // cannot know whether a match is inside, and dropping it asserts there is
    // not one. That was not a theoretical worry: collapsing a group while a
    // filter was in force emptied the screen, with nothing on it to say why.
    //
    // Surviving is the honest answer. The row is still there, still marked
    // closed, and `l` looks inside.
    for i in 0..rows.len() {
        if rows[i].group && !rows[i].expanded {
            keep[i] = true;
        }
    }
    // A group survives if anything it owns survives. Walked from the END so a
    // nested group is already resolved by the time its parent is considered.
    for i in (0..rows.len()).rev() {
        if rows[i].group && !keep[i] {
            keep[i] = owned(rows, i).any(|j| keep[j]);
        }
    }
    (0..rows.len())
        .filter(|&i| keep[i] && (rows[i].selectable || rows[i].group))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> Vec<Row> {
        let group = |label: &str| {
            let mut r = Row::leaf(label);
            r.group = true;
            r.expanded = true;
            r
        };
        let leaf = |label: &str, detail: &str| {
            let mut r = Row::leaf(label).detail(detail);
            r.depth = 1;
            r
        };
        vec![
            group("default"),
            leaf("main", "Merge pull request #85"),
            Row::spacer(),
            group("your branches"),
            leaf("feat/dots", "the dotfiles console"),
            leaf("feat/github-tui", "the root screen"),
            Row::spacer(),
        ]
    }

    #[test]
    fn an_empty_filter_shows_everything_including_spacers() {
        let rows = tree();
        assert_eq!(visible(&rows, ""), (0..rows.len()).collect::<Vec<_>>());
    }

    #[test]
    fn it_matches_the_detail_as_well_as_the_label() {
        let rows = tree();
        // "dotfiles" appears only in feat/dots' subject.
        let v = visible(&rows, "dotfiles");
        let labels: Vec<&str> = v.iter().map(|&i| rows[i].label.as_str()).collect();
        assert!(labels.contains(&"feat/dots"), "{labels:?}");
        assert!(!labels.contains(&"main"), "{labels:?}");
    }

    #[test]
    fn it_ignores_case() {
        let rows = tree();
        assert_eq!(visible(&rows, "FEAT").len(), visible(&rows, "feat").len());
        assert!(visible(&rows, "MERGE")
            .iter()
            .any(|&i| rows[i].label == "main"));
    }

    /// A surviving leaf keeps its heading, or it floats under whatever
    /// happened to remain above it -- which on this screen would file your
    /// branch under "default".
    #[test]
    fn a_matching_leaf_keeps_its_group() {
        let rows = tree();
        let v = visible(&rows, "github-tui");
        let labels: Vec<&str> = v.iter().map(|&i| rows[i].label.as_str()).collect();
        assert_eq!(labels, vec!["your branches", "feat/github-tui"]);
    }

    /// And a group with nothing left under it goes away, rather than sitting
    /// there as an empty heading.
    #[test]
    fn a_group_with_no_survivors_disappears() {
        let rows = tree();
        let labels: Vec<&str> = visible(&rows, "feat")
            .iter()
            .map(|&i| rows[i].label.as_str())
            .collect();
        assert!(!labels.contains(&"default"), "{labels:?}");
        assert!(labels.contains(&"your branches"), "{labels:?}");
    }

    /// A group whose own name matches brings its children with it, because
    /// that is what typing the name of a group means.
    #[test]
    fn matching_a_group_keeps_what_is_under_it() {
        let rows = tree();
        let labels: Vec<&str> = visible(&rows, "your")
            .iter()
            .map(|&i| rows[i].label.as_str())
            .collect();
        assert_eq!(
            labels,
            vec!["your branches", "feat/dots", "feat/github-tui"]
        );
    }

    /// Spacers are a grouping device. Three blank lines between two surviving
    /// rows is not grouping, and navigation already has to skip them.
    #[test]
    fn spacers_drop_out_while_filtering() {
        let rows = tree();
        for &i in &visible(&rows, "feat") {
            assert!(rows[i].selectable || rows[i].group, "a spacer survived");
        }
    }

    #[test]
    fn nothing_matching_yields_nothing() {
        let rows = tree();
        assert!(visible(&rows, "zzzz").is_empty());
    }

    /// The bug that emptied the screen: a collapsed group's children are not
    /// in `rows`, so dropping it claims there is no match inside when the
    /// filter cannot possibly know.
    #[test]
    fn a_collapsed_group_survives_any_filter() {
        let mut rows = tree();
        rows[3].expanded = false;
        rows.truncate(4); // collapsed, so its children were never emitted
        let labels: Vec<&str> = visible(&rows, "zzzz")
            .iter()
            .map(|&i| rows[i].label.as_str())
            .collect();
        assert_eq!(labels, vec!["your branches"], "the screen went blank");
    }

    /// And the cursor lands on what was searched for, not on the heading above
    /// it -- where the next `enter` would collapse the group.
    #[test]
    fn the_cursor_prefers_a_leaf_over_its_heading() {
        let rows = tree();
        let vis = visible(&rows, "github-tui");
        assert!(rows[vis[0]].group, "the heading comes first in the list");
        let at = first_leaf(&rows, &vis);
        assert_eq!(rows[vis[at]].label, "feat/github-tui");
    }

    /// With only headings left there is nothing better to land on, and the
    /// answer must still be a real position.
    #[test]
    fn with_no_leaves_the_cursor_takes_a_heading() {
        let mut rows = tree();
        rows[3].expanded = false;
        rows.truncate(4);
        let vis = visible(&rows, "zzzz");
        assert_eq!(first_leaf(&rows, &vis), 0);
    }

    /// Order is preserved, which is the whole argument for substring over
    /// fzf's ranked fuzzy match: a run list must stay chronological.
    #[test]
    fn order_survives_the_filter() {
        let rows = tree();
        let v = visible(&rows, "feat");
        assert!(v.windows(2).all(|w| w[0] < w[1]), "{v:?}");
    }

    /// The ASCII shortcut in `fold` must not be a different answer from the
    /// general path, only a cheaper one.
    #[test]
    fn folding_is_the_same_with_and_without_ascii() {
        assert_eq!(fold("Feat/GitHub-TUI"), "feat/github-tui");
        assert_eq!(fold("\u{c9}t\u{c9}"), "\u{e9}t\u{e9}");
        let rows = vec![Row::leaf("\u{c9}t\u{e9} 2026")];
        assert_eq!(visible(&rows, "\u{e9}T\u{c9}"), vec![0]);
    }

    /// A needle longer than the row is simply no match.
    #[test]
    fn a_needle_longer_than_the_row_is_simply_no_match() {
        let rows = vec![Row::leaf("ab")];
        assert!(visible(&rows, "abcdef").is_empty());
    }
}
