//! The tier ladder, the LF view, and the offset maps.
//!
//! [`find_matches`] is a pure function: it takes the decoded file text and
//! returns byte ranges into that same text, or a reason why the text may not be
//! edited. It never touches the disk and never allocates the rewritten file.
//!
//! Matching runs on an LF view of the file, because the read tool hands the
//! model LF line endings. Every hit is mapped back to an offset into the
//! original text before it is returned, so [`super::splice`] never needs to know
//! which tier matched. A map costs a few entries per changed stretch, never one
//! per byte, and a file that needs no transform needs no map at all.
//!
//! A UTF-8 BOM is kept out of the searchable text and out of every returned
//! range: the map records it as the floor, so no widening can reach into it.
//! One leading U+FEFF on the needle is removed before any tier runs, so a copy
//! of line 1 still matches when the read tool showed that mark. A mark later
//! in the needle is searched for.

use std::borrow::Cow;
use std::ops::Range;

use super::config::MatchMode;
use super::text_unfold::fold_char;
use crate::text::{Newline, normalize_newlines};

/// UTF-8 byte order mark, kept out of the searchable text.
const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

/// Line numbers [`MatchResult::NotUnique`] reports, and the number of hits a
/// scan without `replace_all` collects.
const MAX_REPORTED_LINES: usize = 10;

/// Largest `lines * needle_len^2` the fuzzy tier runs, which bounds the
/// O(n·m) window scan by one product rather than by a second heuristic.
const FUZZY_COST_LIMIT: u128 = 100_000_000;

/// Score difference below which two windows count as equally good, so neither
/// is chosen.
const FUZZY_MARGIN: f32 = 0.02;

/// Which tier produced a match.
///
/// The tier is reported to the host so it can tell the model that its text was
/// not byte-exact, which means the file should be read again before the next
/// edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchTier {
    /// The needle matched the file or its LF view byte for byte.
    Exact,
    /// The needle matched after per-line trailing whitespace was stripped and
    /// Unicode punctuation was folded.
    Lenient,
    /// The needle matched a window scored at or above the fuzzy threshold.
    Fuzzy,
}

/// One match request.
///
/// The matcher builds the LF view itself, so a caller cannot hand it two
/// strings that disagree about the file content.
pub(crate) struct Search<'a> {
    /// The decoded file text exactly as the host returned it: BOM included,
    /// original line endings intact.
    pub original: &'a str,
    /// The text to find. It must not be empty: an empty needle has no
    /// occurrence identity and is refused before matching.
    pub needle: &'a str,
    /// Which tiers may run.
    pub mode: MatchMode,
    /// Replace every occurrence instead of requiring exactly one.
    pub replace_all: bool,
    /// Largest fan-out a `replace_all` accepts. Ignored when `replace_all` is
    /// false, where counting stops at [`MAX_REPORTED_LINES`].
    pub max_occurrences: usize,
    /// Minimum similarity the fuzzy tier accepts. Read by the fuzzy tier only.
    pub fuzzy_threshold: f32,
}

/// The outcome of one match request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MatchResult {
    /// The edit may be spliced.
    Matched {
        /// Which tier matched.
        tier: MatchTier,
        /// Ascending, non-overlapping byte ranges into `original`, never into
        /// the LF view. One range when `replace_all` is false, one per
        /// occurrence when it is true.
        ranges: Vec<Range<usize>>,
        /// The ending the file uses more often than any other.
        dominant: Newline,
    },
    /// The needle does not occur in any enabled tier.
    NoMatch,
    /// More than one occurrence and `replace_all` is false.
    NotUnique {
        /// Occurrences found, capped at [`MAX_REPORTED_LINES`], so this value
        /// means "this many or more" once [`MatchResult::NotUnique::capped`] is
        /// set.
        count: usize,
        /// One-based line numbers of the occurrences, in file order.
        lines: Vec<usize>,
        /// Whether the scan stopped at the report cap, which makes `count` a
        /// lower bound rather than a proven total.
        capped: bool,
    },
    /// `replace_all` found more occurrences than `max_occurrences`.
    TooMany,
}

/// Finds the ranges `needle` covers in `original`.
///
/// Tiers run in order and the first tier that matches wins: exact always, then
/// the lenient tier unless the mode is [`MatchMode::Exact`], then the fuzzy
/// tier only when the mode is [`MatchMode::Fuzzy`]. A later tier never runs
/// when an earlier one found a hit, so enabling one cannot change a match a
/// cheaper tier already made.
pub(crate) fn find_matches(search: &Search<'_>) -> MatchResult {
    // One leading mark only. A later U+FEFF is part of the text to find.
    let needle = search
        .needle
        .strip_prefix('\u{feff}')
        .unwrap_or(search.needle);
    if needle.is_empty() {
        return MatchResult::NoMatch;
    }
    let search = Search {
        original: search.original,
        needle,
        mode: search.mode,
        replace_all: search.replace_all,
        max_occurrences: search.max_occurrences,
        fuzzy_threshold: search.fuzzy_threshold,
    };
    let view = LfView::build(search.original);
    let exact = exact_matches(&search, &view);
    if !matches!(exact, MatchResult::NoMatch) {
        return exact;
    }
    if search.mode == MatchMode::Exact {
        return MatchResult::NoMatch;
    }

    // The lenient text is also the fuzzy tier's search text, so it is built
    // once for both.
    let lenient = LenientView::build(&view.text);
    let found = lenient_matches(&search, &view, &lenient);
    if !matches!(found, MatchResult::NoMatch) {
        return found;
    }
    if search.mode == MatchMode::Lenient {
        return MatchResult::NoMatch;
    }
    fuzzy_matches(&search, &view, &lenient)
}

/// The largest hit count one scan collects.
///
/// A `replace_all` scan stops one past the cap, which is enough to prove the cap
/// was exceeded. A scan without it stops at the report cap, because that is all
/// a [`MatchResult::NotUnique`] message shows.
fn hit_limit(search: &Search<'_>) -> usize {
    if search.replace_all {
        search.max_occurrences.saturating_add(1)
    } else {
        MAX_REPORTED_LINES
    }
}

/// Runs the exact tier: a raw pass over the original when the needle carries a
/// `\r`, then a pass over the LF view.
///
/// Either pass restarts at the end of the previous hit, so hits never overlap.
/// A raw hit short-circuits: the LF pass runs only when the raw pass found
/// nothing, so a needle can never match through both.
fn exact_matches(search: &Search<'_>, view: &LfView<'_>) -> MatchResult {
    let limit = hit_limit(search);

    if search.needle.as_bytes().contains(&b'\r') {
        let (hits, capped) = scan(search.original, search.needle, view.map.floor, limit);
        if !hits.is_empty() {
            return finish(
                search,
                MatchTier::Exact,
                view.dominant,
                hits,
                capped,
                merge_overlaps,
            );
        }
    }

    let needle = normalize_newlines(search.needle, Newline::Lf);
    let (hits, capped) = scan(&view.text, &needle, 0, limit);
    let hits = hits
        .into_iter()
        .map(|hit| view.map.map_range(hit))
        .collect();
    finish(
        search,
        MatchTier::Exact,
        view.dominant,
        hits,
        capped,
        merge_overlaps,
    )
}

/// Runs the lenient tier: one search over the rstripped, folded LF view.
///
/// The needle is normalized the same way the file was, so a needle copy that
/// lost trailing whitespace or carries curly quotes still matches. Hits are
/// mapped back to original offsets and merged with [`merge_touching`], because
/// widening across the stripped bytes can make two neighbouring hits touch.
fn lenient_matches(search: &Search<'_>, view: &LfView<'_>, lenient: &LenientView) -> MatchResult {
    let needle = lenient_needle(search.needle);
    if needle.is_empty() {
        return MatchResult::NoMatch;
    }

    let (hits, capped) = scan(&lenient.text, &needle, 0, hit_limit(search));
    let hits: Vec<Range<usize>> = hits
        .into_iter()
        .map(|hit| lenient.map_range(hit, view))
        .collect();
    let transformed_len = needle.len();
    finish(
        search,
        MatchTier::Lenient,
        view.dominant,
        hits,
        capped,
        move |hits| merge_touching(hits, transformed_len),
    )
}

/// Applies the uniqueness and fan-out rules to ranges that are already in
/// original offsets.
///
/// `merge` turns the logical hits into the ranges the splice may use, and the
/// caps are checked before it runs, so a fan-out the host configured away is
/// refused whatever the merge would have reported.
fn finish(
    search: &Search<'_>,
    tier: MatchTier,
    dominant: Newline,
    hits: Vec<Range<usize>>,
    capped: bool,
    merge: impl FnOnce(Vec<Range<usize>>) -> Vec<Range<usize>>,
) -> MatchResult {
    if hits.is_empty() {
        return MatchResult::NoMatch;
    }

    if search.replace_all {
        if hits.len() > search.max_occurrences {
            return MatchResult::TooMany;
        }
        return MatchResult::Matched {
            tier,
            ranges: merge(hits),
            dominant,
        };
    }

    if hits.len() > 1 {
        return MatchResult::NotUnique {
            count: hits.len(),
            lines: line_numbers(search.original, &hits),
            capped,
        };
    }

    MatchResult::Matched {
        tier,
        ranges: hits,
        dominant,
    }
}

/// Runs the fuzzy tier: the best-scoring window of the lenient text, scored by
/// normalized Levenshtein similarity.
///
/// `replace_all` does not fan out here. A window has no stable occurrence
/// identity, so at most one window is ever reported and the flag only decides
/// that the caller may accept one replacement.
///
/// The window is only chosen when no other window scores within
/// [`FUZZY_MARGIN`] of it; two near-equal windows are a non-unique match, not a
/// coin flip. Scoring uses the line content, with no trailing newline. The
/// range that is returned includes the last line's terminator, so a needle and
/// a replacement that both end with `\n` replace one terminated line instead
/// of inserting a blank one. The cost guard bounds the whole scan by one
/// product, and nothing else is checked.
fn fuzzy_matches(search: &Search<'_>, view: &LfView<'_>, lenient: &LenientView) -> MatchResult {
    let normalized = lenient_needle(search.needle);
    // Compared as joined lines, so a needle that ends with `\n` scores against
    // the same shape the window has.
    let needle: Vec<&str> = normalized.lines().collect();
    let wanted = needle.len();
    if wanted == 0 {
        return MatchResult::NoMatch;
    }
    let needle = needle.join("\n");

    let count = line_count(&lenient.text);
    if count < wanted || over_fuzzy_budget(count, needle.len()) {
        return MatchResult::NoMatch;
    }
    let lines = line_spans(&lenient.text);

    // Score the content. Splice through the terminator, up to the next line.
    let content = |start: usize| lines[start].0..lines[start + wanted - 1].1;
    let splice_at = |start: usize| {
        let begin = lines[start].0;
        let end = lines
            .get(start + wanted)
            .map(|line| line.0)
            .unwrap_or(lenient.text.len());
        begin..end
    };
    let mut candidates: Vec<(usize, f32)> = Vec::new();
    for start in 0..=count - wanted {
        let score = similarity(&lenient.text[content(start)], &needle);
        if score >= search.fuzzy_threshold {
            candidates.push((start, score));
        }
    }

    let Some(&(best_start, best)) = candidates
        .iter()
        .max_by(|left, right| left.1.total_cmp(&right.1))
    else {
        return MatchResult::NoMatch;
    };

    // shortcut: fixed 0.02 margin, widen if a host needs a knob.
    let close: Vec<usize> = candidates
        .iter()
        .filter(|(_, score)| *score >= best - FUZZY_MARGIN)
        .map(|(start, _)| *start)
        .collect();

    if close.len() > 1 {
        let reported: Vec<Range<usize>> = close
            .iter()
            .take(MAX_REPORTED_LINES)
            .map(|start| lenient.map_range(splice_at(*start), view))
            .collect();
        return MatchResult::NotUnique {
            count: close.len().min(MAX_REPORTED_LINES),
            lines: line_numbers(search.original, &reported),
            capped: close.len() > MAX_REPORTED_LINES,
        };
    }

    MatchResult::Matched {
        tier: MatchTier::Fuzzy,
        ranges: vec![lenient.map_range(splice_at(best_start), view)],
        dominant: view.dominant,
    }
}

/// Returns whether the fuzzy window scan is over its cost budget.
///
/// The scan is O(n·m) per window and runs once per window, so one product
/// bounds it: the line count times the square of the needle's byte length.
fn over_fuzzy_budget(line_count: usize, needle_len: usize) -> bool {
    // shortcut: one product guards the O(n·m) scan, widen the budget if a host needs knob.
    line_count as u128 * (needle_len as u128).pow(2) > FUZZY_COST_LIMIT
}

/// Counts the lines in `text`, the same number [`line_spans`] returns.
///
/// This runs before the cost guard so a hostile needle is refused without
/// allocating one span per line of the file.
fn line_count(text: &str) -> usize {
    let breaks = text.bytes().filter(|byte| *byte == b'\n').count();
    if text.is_empty() || text.ends_with('\n') {
        breaks
    } else {
        breaks + 1
    }
}

/// Returns the span of every line's content in `text`, with no trailing `\n`.
///
/// `text` is the LF view, so a line ends at a `\n` and nowhere else. Joining a
/// window of these spans with `\n` is the same as slicing `text` from the first
/// line's start to the last line's content end.
fn line_spans(text: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let end = text[start..]
            .find('\n')
            .map_or(text.len(), |offset| start + offset);
        spans.push((start, end));
        start = end + 1;
    }
    spans
}

/// Normalized Levenshtein similarity of two strings, counted in characters.
///
/// The score is `1 - distance / max(len_a, len_b)`, so it is `1.0` for two equal
/// strings and for two empty ones.
fn similarity(left: &str, right: &str) -> f32 {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    let longest = left.len().max(right.len());
    if longest == 0 {
        return 1.0;
    }
    let distance = levenshtein(&left, &right);
    1.0 - distance as f32 / longest as f32
}

/// Levenshtein distance between two character slices, with two rolling rows.
fn levenshtein(left: &[char], right: &[char]) -> usize {
    if left.is_empty() {
        return right.len();
    }
    if right.is_empty() {
        return left.len();
    }

    let mut previous: Vec<usize> = (0..=right.len()).collect();
    let mut current = vec![0; right.len() + 1];
    for (row, source) in left.iter().enumerate() {
        current[0] = row + 1;
        for (column, target) in right.iter().enumerate() {
            let substitute = previous[column] + usize::from(source != target);
            current[column + 1] = substitute
                .min(previous[column + 1] + 1)
                .min(current[column] + 1);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

/// Collects non-overlapping hits of `needle` in `text[from..]`, stopping after
/// `limit` hits.
///
/// The offsets are absolute into `text`, not into the searched slice. The flag
/// is true when the scan stopped because it reached `limit`, which is what makes
/// a reported count mean "this many or more".
fn scan(text: &str, needle: &str, from: usize, limit: usize) -> (Vec<Range<usize>>, bool) {
    let mut hits: Vec<Range<usize>> = Vec::new();
    let mut cursor = from;
    while hits.len() < limit {
        let Some(offset) = text[cursor..].find(needle) else {
            return (hits, false);
        };
        let start = cursor + offset;
        let end = start + needle.len();
        hits.push(start..end);
        cursor = end;
    }
    (hits, true)
}

/// Merges ranges that truly overlap, and leaves touching ranges separate.
///
/// Widening during mapping can make two ranges overlap, and splicing an
/// overlapping pair would replace the same original bytes twice.
fn merge_overlaps(hits: Vec<Range<usize>>) -> Vec<Range<usize>> {
    // shortcut: touching ranges stay separate so adjacent exact hits still splice twice; overlap is the double-splice bug.
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(hits.len());
    for hit in hits {
        match merged.last_mut() {
            Some(previous) if hit.start < previous.end => {
                previous.end = previous.end.max(hit.end);
            }
            _ => merged.push(hit),
        }
    }
    merged
}

/// Merges ranges that overlap, and a touching pair when widening ate the
/// separator between them.
///
/// A hit is widened when its mapped length differs from the length it had in the
/// text that was searched. A touch merges only when the right edge of the
/// previous range was widened, because that edge ate the separator. A widened
/// hit earlier in the group must not swallow later byte-adjacent hits.
fn merge_touching(hits: Vec<Range<usize>>, transformed_len: usize) -> Vec<Range<usize>> {
    // shortcut: a touch merges only when widening ate the separator, so a 1:1 adjacent replace_all still splices twice.
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(hits.len());
    // Whether anything merged into the last range of `merged` was widened.
    let mut widened = false;
    for hit in hits {
        let is_widened = hit.len() != transformed_len;
        let touches = match merged.last() {
            Some(previous) => {
                hit.start < previous.end || (hit.start == previous.end && (widened || is_widened))
            }
            None => false,
        };
        if touches {
            let previous = merged.last_mut().expect("a range before the touching one");
            previous.end = previous.end.max(hit.end);
            // The next touch looks at this hit's own edge, not every earlier one.
            widened = is_widened;
        } else {
            merged.push(hit);
            widened = is_widened;
        }
    }
    merged
}

/// Counts `\n` bytes in `original` before each hit start.
///
/// The count is taken in the original, not in the LF view, so the numbers match
/// the lines the read tool shows the model.
fn line_numbers(original: &str, hits: &[Range<usize>]) -> Vec<usize> {
    hits.iter()
        .map(|hit| {
            1 + original[..hit.start]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count()
        })
        .collect()
}

/// The LF view of the searchable text, with the map back to original offsets.
struct LfView<'a> {
    /// The file text with every line ending rewritten to `\n`. Borrowed when
    /// the file has no `\r`, so an LF file costs no allocation.
    text: Cow<'a, str>,
    /// Map from `text` offsets back to `original` offsets.
    map: OffsetMap,
    /// Ending the file uses more often than any other, as `Lf` or `CrLf`.
    dominant: Newline,
}

impl<'a> LfView<'a> {
    /// Builds the view from the decoded file text.
    fn build(original: &'a str) -> Self {
        let bytes = original.as_bytes();
        let floor = if bytes.starts_with(UTF8_BOM) {
            UTF8_BOM.len()
        } else {
            0
        };
        let searchable = &original[floor..];

        if !searchable.as_bytes().contains(&b'\r') {
            return Self {
                text: Cow::Borrowed(searchable),
                map: OffsetMap {
                    spans: Vec::new(),
                    floor,
                    original_len: original.len(),
                },
                dominant: Newline::Lf,
            };
        }

        let source = searchable.as_bytes();
        let mut text = String::with_capacity(source.len());
        let mut spans: Vec<Span> = Vec::new();
        let mut crlf_pairs = 0_usize;
        let mut lone_lf = 0_usize;
        let mut copied = 0_usize;
        let mut index = 0_usize;

        while index < source.len() {
            let byte = source[index];
            if byte != b'\r' && byte != b'\n' {
                index += 1;
                continue;
            }

            push_span(&mut spans, text.len(), copied, index, floor);
            text.push_str(&searchable[copied..index]);

            let (advance, is_crlf) = if byte == b'\r' && source.get(index + 1) == Some(&b'\n') {
                (2, true)
            } else {
                (1, false)
            };
            if is_crlf {
                crlf_pairs += 1;
            } else if byte == b'\n' {
                lone_lf += 1;
            }
            text.push('\n');
            spans.push(Span {
                transformed_start: text.len() - 1,
                transformed_len: 1,
                original_start: index + floor,
                original_len: advance,
            });
            index += advance;
            copied = index;
        }

        push_span(&mut spans, text.len(), copied, source.len(), floor);
        text.push_str(&searchable[copied..]);

        // A tie, or no ending at all, is LF. A lone CR never wins because it
        // counts as neither a CRLF pair nor a lone LF.
        let dominant = if crlf_pairs > lone_lf {
            Newline::CrLf
        } else {
            Newline::Lf
        };

        Self {
            text: Cow::Owned(text),
            map: OffsetMap {
                spans,
                floor,
                original_len: original.len(),
            },
            dominant,
        }
    }
}

/// The rstripped, folded copy of the LF view, with its map back to LF offsets.
///
/// One copy is built per search and shared by the lenient and the fuzzy tier, so
/// neither rebuilds it. The transform keeps the character count, so only a
/// folded character's byte length and a stripped trailing space move an offset.
struct LenientView {
    /// Every line with ASCII space and tab stripped from its end, then every
    /// character folded by [`fold_char`].
    text: String,
    /// Map from `text` offsets back to LF view offsets.
    map: OffsetMap,
}

impl LenientView {
    /// Builds the view from the LF view text.
    fn build(source: &str) -> Self {
        let mut text = String::with_capacity(source.len());
        let mut spans: Vec<Span> = Vec::new();
        let mut changed = false;
        let mut cursor = 0;

        while cursor < source.len() {
            let line_end = source[cursor..]
                .find('\n')
                .map_or(source.len(), |offset| cursor + offset);
            let mut content_end = line_end;
            while content_end > cursor && matches!(source.as_bytes()[content_end - 1], b' ' | b'\t')
            {
                content_end -= 1;
            }

            push_folded(
                &mut text,
                &mut spans,
                &mut changed,
                &source[cursor..content_end],
                cursor,
            );

            // The stripped bytes and the ending share one span, so a hit that
            // reaches the ending widens over the whitespace dropped here. Bytes
            // stripped off the last line of a file add no span: nothing follows
            // them, so no hit can reach past them.
            let newline = usize::from(line_end < source.len());
            if content_end < line_end {
                changed = true;
            }
            if newline == 1 {
                spans.push(Span {
                    transformed_start: text.len(),
                    transformed_len: 1,
                    original_start: content_end,
                    original_len: line_end + 1 - content_end,
                });
                text.push('\n');
            }
            cursor = line_end + newline;
        }

        // An unchanged copy maps 1:1, so it needs no map at all.
        if !changed {
            spans.clear();
        }

        Self {
            text,
            map: OffsetMap {
                spans,
                floor: 0,
                original_len: source.len(),
            },
        }
    }

    /// Maps a range in `text` back to original offsets, through the LF view's
    /// own map.
    fn map_range(&self, range: Range<usize>, view: &LfView<'_>) -> Range<usize> {
        view.map.map_range(self.map.map_range(range))
    }
}

/// Appends `content` with every character folded, recording the spans the map
/// needs.
///
/// `source_start` is where `content` begins in the LF view. A run copied
/// unchanged becomes an identity span; a folded character becomes a span whose
/// two lengths differ, which is what makes a hit over it widen. The spans cover
/// `content` without a gap, because a gap would make an offset land in the wrong
/// run.
fn push_folded(
    text: &mut String,
    spans: &mut Vec<Span>,
    changed: &mut bool,
    content: &str,
    source_start: usize,
) {
    let mut run_start = 0;
    let mut run_out = text.len();
    for (offset, character) in content.char_indices() {
        let folded = fold_char(character);
        if folded == character {
            continue;
        }
        push_span(
            spans,
            run_out,
            source_start + run_start,
            source_start + offset,
            0,
        );
        text.push_str(&content[run_start..offset]);
        spans.push(Span {
            transformed_start: text.len(),
            transformed_len: folded.len_utf8(),
            original_start: source_start + offset,
            original_len: character.len_utf8(),
        });
        text.push(folded);
        *changed = true;
        run_start = offset + character.len_utf8();
        run_out = text.len();
    }
    push_span(
        spans,
        run_out,
        source_start + run_start,
        source_start + content.len(),
        0,
    );
    text.push_str(&content[run_start..]);
}

/// Returns the needle normalized the way [`LenientView`] normalizes the file.
///
/// The ending is converted to LF first, so a needle copied from a CRLF file is
/// normalized once. Trailing spaces and tabs are then stripped per line and
/// every character is folded. Leading whitespace and the line structure are
/// kept, because those are what place a match on its line.
fn lenient_needle(needle: &str) -> String {
    let source = normalize_newlines(needle, Newline::Lf);
    let mut text = String::with_capacity(source.len());
    for line in source.split_inclusive('\n') {
        let (content, ending) = match line.strip_suffix('\n') {
            Some(content) => (content, "\n"),
            None => (line, ""),
        };
        text.extend(content.trim_end_matches([' ', '\t']).chars().map(fold_char));
        text.push_str(ending);
    }
    text
}

/// Records one span where the LF view and the original agree byte for byte.
fn push_span(
    spans: &mut Vec<Span>,
    transformed_start: usize,
    original_start: usize,
    original_end: usize,
    floor: usize,
) {
    if original_end == original_start {
        return;
    }
    spans.push(Span {
        transformed_start,
        transformed_len: original_end - original_start,
        original_start: original_start + floor,
        original_len: original_end - original_start,
    });
}

/// One stretch of a transformed text and the bytes it came from.
///
/// The "transformed" side is whatever text was searched: the LF view for the
/// line-ending map, the folded text for the lenient map.
struct Span {
    /// Offset of the stretch in the transformed text.
    transformed_start: usize,
    /// Length of the stretch in the transformed text.
    transformed_len: usize,
    /// Offset of the source stretch in the text one stage earlier.
    original_start: usize,
    /// Length of the source stretch in the text one stage earlier.
    original_len: usize,
}

impl Span {
    /// First offset in the LF view after this stretch.
    const fn transformed_end(&self) -> usize {
        self.transformed_start + self.transformed_len
    }

    /// First offset in the original after this stretch's source.
    const fn original_end(&self) -> usize {
        self.original_start + self.original_len
    }

    /// Whether the two sides agree byte for byte, so an offset adds up.
    const fn is_identity(&self) -> bool {
        self.transformed_len == self.original_len
    }
}

/// Contiguous spans of a transformed text, and the bytes each came from.
///
/// A span whose two lengths agree maps 1:1 by a constant offset. A span whose
/// lengths differ is one stretch that changed size: a rewritten line ending, or
/// a folded character. An empty list means no byte was rewritten and the view is
/// its source shifted by `floor`, so no map was allocated.
///
/// The lenient map chains onto the line-ending map, so a mapped hit is finalized
/// by the LF map by way of [`LenientView::map_range`].
///
/// Storage is one span per changed stretch plus one per untouched stretch, never
/// one entry per byte.
struct OffsetMap {
    spans: Vec<Span>,
    /// Lowest original offset a mapped range may cover.
    floor: usize,
    /// Length of the original text, the right edge of the last span.
    original_len: usize,
}

impl OffsetMap {
    /// Maps a range in the transformed text to the span that produced it.
    ///
    /// A boundary that falls inside a changed span widens to the whole source
    /// span, so a hit that starts or ends on a rewritten byte still covers the
    /// bytes the source holds.
    fn map_range(&self, range: Range<usize>) -> Range<usize> {
        let start = self.map_start(range.start);
        let end = self.map_end(range.end).max(start);
        start..end
    }

    /// Maps a range start. A boundary inside a rewritten span widens left to the
    /// span's original start, which is never left of [`OffsetMap::floor`].
    fn map_start(&self, offset: usize) -> usize {
        if self.spans.is_empty() {
            return offset + self.floor;
        }
        let index = self
            .spans
            .partition_point(|span| span.transformed_end() <= offset);
        match self.spans.get(index) {
            Some(span) if span.is_identity() => {
                span.original_start + (offset - span.transformed_start)
            }
            Some(span) => span.original_start,
            None => self.original_len,
        }
    }

    /// Maps a range end. A boundary inside a changed span widens right to the
    /// span's source end, so a hit that ends on a rewritten byte covers all of
    /// it.
    fn map_end(&self, offset: usize) -> usize {
        if self.spans.is_empty() {
            return offset + self.floor;
        }
        let index = self
            .spans
            .partition_point(|span| span.transformed_end() < offset);
        match self.spans.get(index) {
            Some(span) if span.is_identity() => {
                span.original_start + (offset - span.transformed_start)
            }
            Some(span) => span.original_end(),
            None => self.original_len,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        LfView, MAX_REPORTED_LINES, MatchResult, MatchTier, OffsetMap, Search, find_matches,
        line_count, line_spans, over_fuzzy_budget,
    };
    use crate::edit::config::{MatchMode, NewlinePolicy};
    use crate::edit::splice::{SpliceError, splice};
    use crate::text::{Newline, normalize_newlines};
    use std::borrow::Cow;
    use std::ops::Range;

    /// Builds an exact-tier request that requires a unique match.
    fn search<'a>(original: &'a str, needle: &'a str) -> Search<'a> {
        Search {
            original,
            needle,
            mode: MatchMode::Exact,
            replace_all: false,
            max_occurrences: 10_000,
            fuzzy_threshold: 0.95,
        }
    }

    /// Asserts the result is a single match and returns its ranges.
    #[track_caller]
    fn ranges(result: MatchResult) -> Vec<Range<usize>> {
        match result {
            MatchResult::Matched { ranges, .. } => ranges,
            other => panic!("expected a match, got {other:?}"),
        }
    }

    /// Asserts the result is a match and returns the ranges with the dominant
    /// ending.
    #[track_caller]
    fn matched(result: MatchResult) -> (Vec<Range<usize>>, Newline) {
        match result {
            MatchResult::Matched {
                tier,
                ranges,
                dominant,
            } => {
                assert_eq!(tier, MatchTier::Exact);
                (ranges, dominant)
            }
            other => panic!("expected a match, got {other:?}"),
        }
    }

    /// Reduces ranges to `(start, end)` pairs, so a one-range assertion does
    /// not build a one-element range collection.
    fn spans(ranges: &[Range<usize>]) -> Vec<(usize, usize)> {
        ranges
            .iter()
            .map(|range| (range.start, range.end))
            .collect()
    }

    /// Covers each hit with the original text it maps to.
    fn covered<'a>(original: &'a str, ranges: &[Range<usize>]) -> Vec<&'a str> {
        ranges
            .iter()
            .map(|range| &original[range.clone()])
            .collect()
    }

    #[test]
    fn exact_match_at_start_middle_and_end() {
        let file = "alpha\nbeta\ngamma\n";
        assert_eq!(
            spans(&ranges(find_matches(&search(file, "alpha")))),
            [(0, 5)]
        );
        assert_eq!(
            spans(&ranges(find_matches(&search(file, "beta")))),
            [(6, 10)]
        );
        assert_eq!(
            spans(&ranges(find_matches(&search(file, "gamma")))),
            [(11, 16)]
        );
    }

    #[test]
    fn exact_match_on_a_last_line_without_a_trailing_newline() {
        let file = "alpha\nbeta";
        let found = ranges(find_matches(&search(file, "beta")));
        assert_eq!(spans(&found), [(6, 10)]);
        assert_eq!(covered(file, &found), ["beta"]);
    }

    #[test]
    fn a_crlf_file_is_matched_through_the_lf_view_and_maps_back() {
        let file = "one\r\ntwo\r\nthree\r\n";
        let (found, dominant) = matched(find_matches(&search(file, "two")));
        assert_eq!(spans(&found), [(5, 8)]);
        assert_eq!(covered(file, &found), ["two"]);
        assert_eq!(dominant, Newline::CrLf);

        // A multi-line needle reaches across a rewritten ending.
        let (found, _) = matched(find_matches(&search(file, "one\r\ntwo")));
        assert_eq!(covered(file, &found), ["one\r\ntwo"]);
    }

    #[test]
    fn a_needle_carrying_crlf_matches_a_crlf_file_on_the_raw_pass() {
        let file = "one\r\ntwo\r\nthree\r\n";
        // The raw pass reports original offsets, so the hit keeps its `\r\n`.
        let (found, dominant) = matched(find_matches(&search(file, "two\r\nthree")));
        assert_eq!(spans(&found), [(5, 15)]);
        assert_eq!(covered(file, &found), ["two\r\nthree"]);
        assert_eq!(dominant, Newline::CrLf);
    }

    #[test]
    fn mixed_endings_map_back_to_the_bytes_the_file_has() {
        let file = "a\r\nb\nc\r\n";
        let (found, dominant) = matched(find_matches(&search(file, "b")));
        assert_eq!(spans(&found), [(3, 4)]);
        assert_eq!(dominant, Newline::CrLf);

        // `b\nc` sits between an LF and a CRLF ending: the hit keeps both.
        let (found, _) = matched(find_matches(&search(file, "b\nc")));
        assert_eq!(covered(file, &found), ["b\nc"]);
    }

    #[test]
    fn a_tie_between_endings_is_lf() {
        let file = "a\r\nb\nc";
        let (_, dominant) = matched(find_matches(&search(file, "a")));
        assert_eq!(dominant, Newline::Lf);
    }

    #[test]
    fn a_lone_cr_never_wins_the_dominant_ending() {
        let file = "a\rb\r\nc\n";
        let (_, dominant) = matched(find_matches(&search(file, "a")));
        assert_eq!(dominant, Newline::Lf);

        let file = "a\rb\r\nc";
        let (_, dominant) = matched(find_matches(&search(file, "a")));
        assert_eq!(dominant, Newline::CrLf);
    }

    #[test]
    fn a_bom_file_matches_line_one_and_never_widens_over_the_bom() {
        let file = "\u{feff}first\nsecond\n";
        let (found, dominant) = matched(find_matches(&search(file, "first")));
        assert_eq!(covered(file, &found), ["first"]);
        assert_eq!(dominant, Newline::Lf);

        let view = LfView::build(file);
        assert_eq!(view.map.floor, 3);
        assert!(found.iter().all(|range| range.start >= 3));
        assert!(view.text.starts_with("first"));

        // A CRLF file with a BOM keeps the offset map's runs above the BOM.
        let file = "\u{feff}one\r\ntwo\r\n";
        let (found, _) = matched(find_matches(&search(file, "one\r\ntwo")));
        assert!(found.iter().all(|range| range.start >= 3));
        assert_eq!(covered(file, &found), ["one\r\ntwo"]);
    }

    #[test]
    fn a_leading_mark_on_the_needle_still_matches_line_one() {
        let file = "\u{feff}first\nsecond\n";
        let (found, dominant) = matched(find_matches(&search(file, "\u{feff}first")));
        assert_eq!(covered(file, &found), ["first"]);
        assert!(found.iter().all(|range| range.start >= 3));
        let output = splice(
            file,
            &found,
            "FIRST",
            NewlinePolicy::Preserve,
            dominant,
            1 << 20,
        )
        .expect("within budget");
        assert!(output.starts_with('\u{feff}'));
        assert_eq!(output.matches('\u{feff}').count(), 1);
        assert_eq!(output, "\u{feff}FIRST\nsecond\n");

        // A mark that is not the first character is text, not a prefix to drop.
        assert_eq!(
            find_matches(&search(file, "fi\u{feff}rst")),
            MatchResult::NoMatch
        );
        assert_eq!(
            find_matches(&search(file, "\u{feff}")),
            MatchResult::NoMatch
        );
    }

    #[test]
    fn two_occurrences_are_not_unique_with_both_line_numbers() {
        let file = "alpha\nbeta\nalpha\n";
        match find_matches(&search(file, "alpha")) {
            MatchResult::NotUnique {
                count,
                lines,
                capped,
            } => {
                assert_eq!(count, 2);
                assert_eq!(lines, [1, 3]);
                assert!(!capped);
            }
            other => panic!("expected a non-unique match, got {other:?}"),
        }
    }

    #[test]
    fn line_numbers_count_newlines_in_the_original_not_in_the_lf_view() {
        // A lone CR is not a line break to the read tool, so it is not one here
        // even though the LF view turns it into one.
        let file = "x\rx\nx\n";
        match find_matches(&search(file, "x")) {
            MatchResult::NotUnique {
                count,
                lines,
                capped,
            } => {
                assert_eq!(count, 3);
                assert_eq!(lines, [1, 1, 2]);
                assert!(!capped);
            }
            other => panic!("expected a non-unique match, got {other:?}"),
        }
    }

    #[test]
    fn a_scan_without_replace_all_reports_a_capped_count() {
        let file = "x\n".repeat(MAX_REPORTED_LINES + 5);
        match find_matches(&search(&file, "x")) {
            MatchResult::NotUnique {
                count,
                lines,
                capped,
            } => {
                assert_eq!(count, MAX_REPORTED_LINES);
                assert_eq!(lines.len(), MAX_REPORTED_LINES);
                assert!(capped);
            }
            other => panic!("expected a capped count, got {other:?}"),
        }
    }

    #[test]
    fn replace_all_of_two_occurrences_gives_two_ranges() {
        let file = "x\ny\nx\n";
        let mut request = search(file, "x");
        request.replace_all = true;
        let found = ranges(find_matches(&request));
        assert_eq!(spans(&found), [(0, 1), (4, 5)]);
        assert_eq!(covered(file, &found), ["x", "x"]);
    }

    #[test]
    fn replace_all_stops_one_hit_past_the_cap_and_reports_too_many() {
        let file = "x\nx\nx\n";
        let mut request = search(file, "x");
        request.replace_all = true;
        request.max_occurrences = 2;
        assert_eq!(find_matches(&request), MatchResult::TooMany);

        request.max_occurrences = 3;
        assert_eq!(ranges(find_matches(&request)).len(), 3);
    }

    #[test]
    fn aa_in_aaa_is_one_non_overlapping_hit() {
        let file = "aaa";
        assert_eq!(spans(&ranges(find_matches(&search(file, "aa")))), [(0, 2)]);
    }

    #[test]
    fn aa_in_aaaa_with_replace_all_is_two_touching_ranges() {
        let file = "aaaa";
        let mut request = search(file, "aa");
        request.replace_all = true;
        let found = ranges(find_matches(&request));
        assert_eq!(spans(&found), [(0, 2), (2, 4)]);
    }

    /// A twenty-character needle, so one substitution scores exactly 0.95.
    const FUZZY_NEEDLE: &str = "abcdefghij0123456789";

    #[test]
    fn the_fuzzy_tier_is_not_consulted_outside_fuzzy_mode() {
        // One substitution over twenty characters scores exactly 0.95, so only
        // the mode keeps this from matching.
        let file = "abcdefghij0123456788\n";
        let mut request = search(file, FUZZY_NEEDLE);
        request.mode = MatchMode::Fuzzy;
        assert!(matches!(
            find_matches(&request),
            MatchResult::Matched {
                tier: MatchTier::Fuzzy,
                ..
            }
        ));

        for mode in [MatchMode::Exact, MatchMode::Lenient] {
            let mut request = search(file, FUZZY_NEEDLE);
            request.mode = mode;
            assert_eq!(find_matches(&request), MatchResult::NoMatch);
        }
    }

    #[test]
    fn a_trailing_space_miss_is_lenient_and_keeps_the_original_indentation() {
        // The file line carries trailing spaces the model's copy does not, so
        // only the lenient tier finds it.
        let file = "  let value = 1;   \n  keep();\n";
        let mut request = search(file, "let value = 1;\n");
        assert_eq!(find_matches(&request), MatchResult::NoMatch);

        request.mode = MatchMode::Lenient;
        match find_matches(&request) {
            MatchResult::Matched { tier, ranges, .. } => {
                assert_eq!(tier, MatchTier::Lenient);
                assert_eq!(covered(file, &ranges), ["let value = 1;   \n"]);
                let output = splice(
                    file,
                    &ranges,
                    "let value = 2;\n",
                    NewlinePolicy::Preserve,
                    Newline::Lf,
                    1 << 20,
                )
                .expect("within budget");
                // The two leading spaces are outside the match, so they stay.
                assert_eq!(output, "  let value = 2;\n  keep();\n");
            }
            other => panic!("expected a lenient match, got {other:?}"),
        }

        // The reverse direction works too: a needle that still has the
        // trailing spaces matches a file line that lost them.
        let file = "  let value = 1;\n";
        let mut request = search(file, "let value = 1;   \n");
        request.mode = MatchMode::Lenient;
        assert!(matches!(
            find_matches(&request),
            MatchResult::Matched {
                tier: MatchTier::Lenient,
                ..
            }
        ));
    }

    #[test]
    fn a_folded_quote_miss_is_lenient() {
        let file = "print(\"hi\")\n";
        let mut request = search(file, "print(\u{201C}hi\u{201D})");
        request.mode = MatchMode::Lenient;
        match find_matches(&request) {
            MatchResult::Matched { tier, ranges, .. } => {
                assert_eq!(tier, MatchTier::Lenient);
                assert_eq!(covered(file, &ranges), ["print(\"hi\")"]);
            }
            other => panic!("expected a lenient match, got {other:?}"),
        }
    }

    #[test]
    fn a_lenient_hit_maps_back_through_both_maps() {
        // A BOM, CRLF endings, trailing spaces, and a curly quote all move an
        // offset, and the hit still covers exactly the bytes the file holds.
        let file = "\u{feff}one  \r\ntw\u{2019}o  \r\nthree\r\n";
        let mut request = search(file, "one\ntw'o\n");
        request.mode = MatchMode::Lenient;
        match find_matches(&request) {
            MatchResult::Matched { tier, ranges, .. } => {
                assert_eq!(tier, MatchTier::Lenient);
                assert_eq!(covered(file, &ranges), ["one  \r\ntw\u{2019}o  \r\n"]);
            }
            other => panic!("expected a lenient match, got {other:?}"),
        }
    }

    #[test]
    fn a_lenient_replace_all_merges_ranges_that_widening_made_touch() {
        // Both hits widen over the bytes `rstrip` dropped, so they touch in the
        // original and must splice once rather than replacing shared bytes.
        let file = "foo   \nfoo   \n";
        let mut request = search(file, "foo\n");
        request.mode = MatchMode::Lenient;
        request.replace_all = true;
        let (found, dominant) = match find_matches(&request) {
            MatchResult::Matched {
                tier,
                ranges,
                dominant,
            } => {
                assert_eq!(tier, MatchTier::Lenient);
                (ranges, dominant)
            }
            other => panic!("expected a lenient match, got {other:?}"),
        };
        assert_eq!(spans(&found), [(0, 14)]);
        let output = splice(
            file,
            &found,
            "bar\n",
            NewlinePolicy::Preserve,
            dominant,
            1 << 20,
        )
        .expect("within budget");
        assert_eq!(output, "bar\n");
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn a_widened_lenient_hit_does_not_swallow_later_adjacent_hits() {
        // The first hit widens over trailing spaces. The next two are adjacent
        // byte for byte, so only the widened edge may merge with the one beside it.
        let file = "a   \na\na\n";
        let mut request = search(file, "a \n");
        request.mode = MatchMode::Lenient;
        request.replace_all = true;
        let (found, dominant) = match find_matches(&request) {
            MatchResult::Matched {
                ranges, dominant, ..
            } => (ranges, dominant),
            other => panic!("expected a lenient match, got {other:?}"),
        };
        assert_eq!(spans(&found), [(0, 7), (7, 9)]);
        let output = splice(
            file,
            &found,
            "Z\n",
            NewlinePolicy::Preserve,
            dominant,
            1 << 20,
        )
        .expect("within budget");
        assert_eq!(output, "Z\nZ\n");
        assert_eq!(found.len(), 2);
    }

    #[test]
    fn a_lenient_replace_all_keeps_one_to_one_adjacent_hits_separate() {
        // No byte was dropped or folded here, so the two hits are adjacent byte
        // for byte and each still splices on its own.
        let file = "aaaa";
        let mut request = search(file, "aa");
        request.mode = MatchMode::Lenient;
        request.replace_all = true;
        assert_eq!(spans(&ranges(find_matches(&request))), [(0, 2), (2, 4)]);
    }

    #[test]
    fn a_fuzzy_window_at_the_threshold_matches_and_below_it_does_not() {
        // One substitution over twenty characters is exactly 1 - 1/20.
        let near = "abcdefghij0123456788\n";
        let mut request = search(near, FUZZY_NEEDLE);
        request.mode = MatchMode::Fuzzy;
        match find_matches(&request) {
            MatchResult::Matched { tier, ranges, .. } => {
                assert_eq!(tier, MatchTier::Fuzzy);
                assert_eq!(covered(near, &ranges), ["abcdefghij0123456788\n"]);
            }
            other => panic!("expected a fuzzy match, got {other:?}"),
        }

        // Two substitutions score 0.9, below the default threshold.
        let far = "abcdefghij0123456700\n";
        let mut request = search(far, FUZZY_NEEDLE);
        request.mode = MatchMode::Fuzzy;
        assert_eq!(find_matches(&request), MatchResult::NoMatch);
    }

    #[test]
    fn a_fuzzy_window_replaces_the_terminated_line() {
        // Both the needle and the replacement end with a newline. The range
        // includes the file's terminator, so the replacement does not add one.
        let file = "abcdefghij0123456788\nkeep\n";
        let mut request = search(file, "abcdefghij0123456789\n");
        request.mode = MatchMode::Fuzzy;
        let (found, dominant) = match find_matches(&request) {
            MatchResult::Matched {
                tier,
                ranges,
                dominant,
            } => {
                assert_eq!(tier, MatchTier::Fuzzy);
                (ranges, dominant)
            }
            other => panic!("expected a fuzzy match, got {other:?}"),
        };
        assert_eq!(covered(file, &found), ["abcdefghij0123456788\n"]);
        let output = splice(
            file,
            &found,
            "Z\n",
            NewlinePolicy::Preserve,
            dominant,
            1 << 20,
        )
        .expect("within budget");
        assert_eq!(output, "Z\nkeep\n");
    }

    #[test]
    fn two_fuzzy_windows_at_the_threshold_are_not_unique() {
        let file = "abcdefghij0123456780\nabcdefghij0123456781\n";
        let mut request = search(file, FUZZY_NEEDLE);
        request.mode = MatchMode::Fuzzy;
        match find_matches(&request) {
            MatchResult::NotUnique { count, lines, .. } => {
                assert_eq!(count, 2);
                assert_eq!(lines, [1, 2]);
            }
            other => panic!("expected a non-unique fuzzy match, got {other:?}"),
        }
    }

    #[test]
    fn a_fuzzy_scan_over_the_cost_guard_is_no_match() {
        // One line and a 10001-byte needle: the product is over 1e8, so the
        // guard refuses the scan even though a window scores 0.9999.
        let line = "a".repeat(10_000);
        let file = format!("{line}b\n");
        let needle = "a".repeat(10_001);
        let mut request = search(&file, &needle);
        request.mode = MatchMode::Fuzzy;
        assert_eq!(find_matches(&request), MatchResult::NoMatch);
    }

    #[test]
    fn the_fuzzy_cost_guard_is_the_one_product() {
        assert!(!over_fuzzy_budget(1, 10_000));
        assert!(over_fuzzy_budget(1, 10_001));
        assert!(!over_fuzzy_budget(0, 1_000_000));
    }

    #[test]
    fn the_line_count_matches_the_spans_it_guards() {
        for text in ["", "\n", "\n\n", "a", "a\n", "a\nb", "a\nb\n"] {
            assert_eq!(line_count(text), line_spans(text).len(), "{text:?}");
        }
    }

    #[test]
    fn a_lenient_fan_out_over_the_cap_is_too_many_before_any_merge() {
        // Two logical hits merge into one range, and the cap still counts the
        // logical hits the scan found, so the fan-out is refused.
        let file = "foo   \nfoo   \n";
        let mut request = search(file, "foo\n");
        request.mode = MatchMode::Lenient;
        request.replace_all = true;
        request.max_occurrences = 1;
        assert_eq!(find_matches(&request), MatchResult::TooMany);

        request.max_occurrences = 2;
        assert_eq!(ranges(find_matches(&request)).len(), 1);
    }

    #[test]
    fn a_missing_needle_is_no_match() {
        assert_eq!(find_matches(&search("alpha", "beta")), MatchResult::NoMatch);
        assert_eq!(find_matches(&search("", "beta")), MatchResult::NoMatch);
    }

    /// Deterministic generator, so a failure is reproducible without a crate.
    struct Lcg(u64);

    impl Lcg {
        fn below(&mut self, bound: usize) -> usize {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((self.0 >> 33) as usize) % bound
        }
    }

    /// Builds a short file from pieces that force every ending path.
    fn generated_original(lcg: &mut Lcg) -> String {
        const PIECES: [&str; 6] = ["a", "bc", "\r\n", "\n", "\r", "de"];
        let mut text = String::new();
        for _ in 0..lcg.below(10) {
            text.push_str(PIECES[lcg.below(PIECES.len())]);
        }
        if lcg.below(2) == 0 {
            text.insert(0, '\u{feff}');
        }
        text
    }

    #[test]
    fn mapping_a_transformed_range_back_normalizes_to_the_transformed_slice() {
        let mut lcg = Lcg(0x5EED);
        for _ in 0..64 {
            let original = generated_original(&mut lcg);
            let view = LfView::build(&original);
            let aligned: Vec<usize> = (0..=view.text.len())
                .filter(|offset| view.map.map_start(*offset) == view.map.map_end(*offset))
                .collect();
            assert!(!aligned.is_empty());

            let first = aligned[lcg.below(aligned.len())];
            let second = aligned[lcg.below(aligned.len())];
            let (start, end) = (first.min(second), first.max(second));

            let mapped = view.map.map_range(start..end);
            assert!(
                mapped.start >= view.map.floor,
                "a mapped range covered the BOM"
            );
            assert!(mapped.end <= original.len());

            let recovered = normalize_newlines(&original[mapped], Newline::Lf);
            assert_eq!(recovered.as_ref(), &view.text[start..end]);
        }
    }

    #[test]
    fn a_hit_that_starts_on_a_rewritten_ending_widens_to_the_whole_ending() {
        // The LF view turns each `\r\n` into a single `\n`, so a needle that
        // starts on one maps to the pair of bytes the file actually holds.
        let file = "ab\r\ncd\r\n";
        let (found, dominant) = matched(find_matches(&search(file, "\ncd")));
        assert_eq!(covered(file, &found), ["\r\ncd"]);
        assert_eq!(dominant, Newline::CrLf);
    }

    #[test]
    fn a_hit_ending_at_the_end_of_a_crlf_file_covers_the_last_ending() {
        let file = "ab\r\n";
        let (found, _) = matched(find_matches(&search(file, "\n")));
        assert_eq!(covered(file, &found), ["\r\n"]);
    }

    /// Runs the matcher and then the splice, the way the edit tool will.
    fn edit(original: &str, needle: &str, replacement: &str, newline: NewlinePolicy) -> String {
        let (ranges, dominant) = matched(find_matches(&search(original, needle)));
        splice(original, &ranges, replacement, newline, dominant, 1 << 20).expect("within budget")
    }

    #[test]
    fn a_crlf_file_spliced_keeps_crlf_outside_the_region() {
        let original = "one\r\ntwo\r\nthree\r\n";
        let output = edit(original, "two", "changed", NewlinePolicy::Preserve);
        assert_eq!(output, "one\r\nchanged\r\nthree\r\n");
        // No lone LF was introduced anywhere.
        assert_eq!(output.matches("\r\n").count(), 3);
    }

    #[test]
    fn mixed_endings_outside_the_region_stay_identical() {
        let original = "a\r\nb\nc\r\nd\r\n";
        let output = edit(original, "b", "B", NewlinePolicy::Preserve);
        assert_eq!(output, "a\r\nB\nc\r\nd\r\n");
        assert_eq!(&output[..3], &original[..3]);
        assert_eq!(&output[4..], &original[4..]);
    }

    #[test]
    fn a_bom_file_spliced_still_starts_with_exactly_one_bom() {
        let original = "\u{feff}first\nsecond\n";
        let output = edit(original, "first", "changed", NewlinePolicy::Preserve);
        assert_eq!(output, "\u{feff}changed\nsecond\n");
        assert_eq!(output.matches('\u{feff}').count(), 1);
    }

    #[test]
    fn replace_all_of_two_occurrences_splices_two_replacements() {
        let original = "x\nkeep\nx\n";
        let mut request = search(original, "x");
        request.replace_all = true;
        let (ranges, dominant) = matched(find_matches(&request));
        let output = splice(
            original,
            &ranges,
            "y",
            NewlinePolicy::Preserve,
            dominant,
            1 << 20,
        )
        .expect("within budget");
        assert_eq!(output, "y\nkeep\ny\n");
        assert_eq!(ranges.len(), 2);
    }

    #[test]
    fn a_spliced_result_over_budget_is_refused() {
        let original = "abc";
        let mut request = search(original, "abc");
        request.replace_all = true;
        let (ranges, dominant) = matched(find_matches(&request));
        let error = splice(
            original,
            &ranges,
            &"y".repeat(64),
            NewlinePolicy::Preserve,
            dominant,
            8,
        )
        .expect_err("64 bytes is over an 8-byte budget");
        assert_eq!(
            error,
            SpliceError::TooLarge {
                limit: 8,
                bytes: 64
            }
        );
    }

    #[test]
    fn a_bom_only_file_shifts_without_a_map() {
        let view = LfView::build("\u{feff}abc");
        assert!(matches!(view.text, Cow::Borrowed(_)));
        assert!(view.map.spans.is_empty());
        assert_eq!(view.map.map_start(0), 3);
        assert_eq!(view.map.map_end(3), 6);
    }

    #[test]
    fn an_empty_map_is_the_view_shifted_by_the_floor() {
        let map = OffsetMap {
            spans: Vec::new(),
            floor: 3,
            original_len: 10,
        };
        assert_eq!(map.map_range(2..4), 5..7);
        assert_eq!(map.map_start(0), 3);
        assert_eq!(map.map_end(7), 10);
    }
}
