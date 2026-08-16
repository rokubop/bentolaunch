//! Type-to-filter matching. Pure arithmetic on strings, so the rules are
//! testable without a window.
//!
//! Two decisions shape everything here.
//!
//! **Filtering hides, it never reorders.** A tile that survives the query stays
//! in its section, in that section's own order — MRU for windows, the pinned
//! order for the rest. The grid's positions are the thing that makes it
//! learnable (DESIGN.md, "Resolved"), and a list that re-sorts on every
//! keystroke throws that away. The score below exists only to decide which
//! surviving tile the keyboard selection starts on.
//!
//! **Every term must match.** A query splits on whitespace and each term is
//! required, so typing more always narrows. `chr git` finds the Chrome window
//! about a repo, not every Chrome window plus every git one.
//!
//! Matching is prefix and substring only. Subsequence ("fuzzy") matching was
//! left out on purpose: over ~60 tiles it mostly widens the result set with
//! matches the user cannot predict, which is the opposite of what filtering is
//! for here.

/// Title matched from its first character.
const TITLE_PREFIX: u32 = 100;
/// A word inside the title started with the term.
const WORD_PREFIX: u32 = 80;
/// The term appears somewhere in the title.
const TITLE_SUBSTRING: u32 = 55;
/// The detail line — a process name, or a shortened path — starts with it.
const DETAIL_PREFIX: u32 = 30;
const DETAIL_SUBSTRING: u32 = 20;

/// `None` when the item does not match. An empty query matches everything with
/// a score of 0, which keeps the caller from special-casing it.
pub fn score(query: &str, title: &str, detail: &str) -> Option<u32> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(str::to_lowercase)
        .collect();
    if terms.is_empty() {
        return Some(0);
    }

    let title = title.to_lowercase();
    let detail = detail.to_lowercase();

    let mut total = 0u32;
    for term in &terms {
        total += term_score(term, &title, &detail)?;
    }
    Some(total)
}

/// Best score this one term can claim, or `None` if it is absent from both
/// lines. Ordered strongest first and returns on the first hit, so a term never
/// scores twice for the same item.
fn term_score(term: &str, title: &str, detail: &str) -> Option<u32> {
    if title.starts_with(term) {
        return Some(TITLE_PREFIX);
    }
    if starts_a_word(title, term) {
        return Some(WORD_PREFIX);
    }
    if title.contains(term) {
        return Some(TITLE_SUBSTRING);
    }
    if detail.starts_with(term) {
        return Some(DETAIL_PREFIX);
    }
    if detail.contains(term) {
        return Some(DETAIL_SUBSTRING);
    }
    None
}

/// Does any word in `text` begin with `term`?
///
/// Window titles are punctuation-heavy — `DESIGN.md - flick - Code`,
/// `R:\dev\flick` — so a word is any run of alphanumerics, not just something
/// with spaces around it. That is what lets `flick` find both of those.
fn starts_a_word(text: &str, term: &str) -> bool {
    text.split(|c: char| !c.is_alphanumeric())
        .any(|word| word.starts_with(term))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sugar: did it match at all?
    fn hit(query: &str, title: &str) -> bool {
        score(query, title, "").is_some()
    }

    #[test]
    fn an_empty_query_keeps_everything() {
        assert_eq!(score("", "anything", "at all"), Some(0));
        assert_eq!(score("   ", "anything", "at all"), Some(0));
    }

    #[test]
    fn matching_ignores_case_in_both_directions() {
        assert!(hit("CHROME", "chrome"));
        assert!(hit("chrome", "CHROME"));
    }

    #[test]
    fn a_term_absent_from_both_lines_is_a_miss() {
        assert_eq!(score("zzz", "Chrome", "chrome.exe"), None);
    }

    #[test]
    fn every_term_has_to_match() {
        assert!(hit("design flick", "DESIGN.md - flick - Code"));
        // Second term is nowhere in the title: the whole query misses.
        assert!(!hit("design zzz", "DESIGN.md - flick - Code"));
    }

    #[test]
    fn typing_more_only_ever_narrows() {
        let titles = [
            "Chrome",
            "DESIGN.md - flick - Code",
            "flick - File Explorer",
            "Spotify",
        ];
        let mut previous = titles.len() + 1;
        for query in ["", "f", "fl", "flick", "flick code"] {
            let count = titles.iter().filter(|t| hit(query, t)).count();
            assert!(
                count <= previous,
                "\"{query}\" matched {count}, more than the shorter query's {previous}"
            );
            previous = count;
        }
        assert_eq!(previous, 1);
    }

    #[test]
    fn a_word_inside_a_punctuated_title_is_reachable() {
        // The cases that made `starts_a_word` split on non-alphanumerics.
        assert!(hit("flick", "DESIGN.md - flick - Code"));
        assert!(hit("flick", r"R:\dev\flick"));
        assert!(hit("md", "DESIGN.md - flick - Code"));
    }

    #[test]
    fn the_detail_line_is_searchable_too() {
        // Nothing in the title says "chrome"; the process name does.
        assert!(score("chrome", "Wikipedia", "chrome.exe").is_some());
        assert!(score("chrome", "Wikipedia", "").is_none());
    }

    #[test]
    fn a_title_prefix_outranks_everything_it_could_be_confused_with() {
        let prefix = score("code", "Code", "").unwrap();
        let word = score("code", "flick - Code", "").unwrap();
        let substring = score("code", "encoder", "").unwrap();
        let detail = score("code", "Wikipedia", "code.exe").unwrap();
        assert!(prefix > word, "{prefix} !> {word}");
        assert!(word > substring, "{word} !> {substring}");
        assert!(substring > detail, "{substring} !> {detail}");
    }

    #[test]
    fn a_term_scores_once_even_when_it_matches_several_ways() {
        // "code" is both the title's prefix and a word start; it must not stack.
        assert_eq!(score("code", "Code - code", ""), Some(TITLE_PREFIX));
    }

    #[test]
    fn more_matching_terms_score_higher_than_fewer() {
        let one = score("design", "DESIGN.md - flick", "").unwrap();
        let two = score("design flick", "DESIGN.md - flick", "").unwrap();
        assert!(two > one);
    }

    #[test]
    fn a_term_may_be_split_across_the_two_lines() {
        // "flick" from the title, "chrome" from the process name.
        assert!(score("flick chrome", "flick - the grid", "chrome.exe").is_some());
    }

    #[test]
    fn punctuation_in_the_query_is_matched_literally() {
        assert!(hit("design.md", "DESIGN.md - flick"));
        assert!(hit(r"r:\dev", r"R:\dev\flick"));
    }
}
