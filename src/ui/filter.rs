//! Type-to-filter matching.
//!
//! Hides, never reorders: stable tile positions are what make the grid
//! learnable. Score only decides where the keyboard selection starts.
//! Every term must match, so typing always narrows.
//! No fuzzy subsequence: it adds matches you cannot predict.

const TITLE_PREFIX: u32 = 100;
const WORD_PREFIX: u32 = 80;
const TITLE_SUBSTRING: u32 = 55;
const DETAIL_PREFIX: u32 = 30;
const DETAIL_SUBSTRING: u32 = 20;

/// Empty query matches everything at 0, so callers need no special case.
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

/// Strongest match wins. A term never scores twice for the same item.
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

/// A word is any run of alphanumerics. Window titles are punctuation-heavy:
/// `DESIGN.md - bentopick - Code`, `R:\dev\bentopick`.
fn starts_a_word(text: &str, term: &str) -> bool {
    text.split(|c: char| !c.is_alphanumeric())
        .any(|word| word.starts_with(term))
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(hit("design bentopick", "DESIGN.md - bentopick - Code"));
        assert!(!hit("design zzz", "DESIGN.md - bentopick - Code"));
    }

    #[test]
    fn typing_more_only_ever_narrows() {
        let titles = [
            "Chrome",
            "DESIGN.md - bentopick - Code",
            "bentopick - File Explorer",
            "Spotify",
        ];
        let mut previous = titles.len() + 1;
        for query in ["", "b", "be", "bentopick", "bentopick code"] {
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
        assert!(hit("bentopick", "DESIGN.md - bentopick - Code"));
        assert!(hit("bentopick", r"R:\dev\bentopick"));
        assert!(hit("md", "DESIGN.md - bentopick - Code"));
    }

    #[test]
    fn the_detail_line_is_searchable_too() {
        assert!(score("chrome", "Wikipedia", "chrome.exe").is_some());
        assert!(score("chrome", "Wikipedia", "").is_none());
    }

    #[test]
    fn a_title_prefix_outranks_everything_it_could_be_confused_with() {
        let prefix = score("code", "Code", "").unwrap();
        let word = score("code", "bentopick - Code", "").unwrap();
        let substring = score("code", "encoder", "").unwrap();
        let detail = score("code", "Wikipedia", "code.exe").unwrap();
        assert!(prefix > word, "{prefix} !> {word}");
        assert!(word > substring, "{word} !> {substring}");
        assert!(substring > detail, "{substring} !> {detail}");
    }

    #[test]
    fn a_term_scores_once_even_when_it_matches_several_ways() {
        assert_eq!(score("code", "Code - code", ""), Some(TITLE_PREFIX));
    }

    #[test]
    fn more_matching_terms_score_higher_than_fewer() {
        let one = score("design", "DESIGN.md - bentopick", "").unwrap();
        let two = score("design bentopick", "DESIGN.md - bentopick", "").unwrap();
        assert!(two > one);
    }

    #[test]
    fn a_term_may_be_split_across_the_two_lines() {
        assert!(score("bentopick chrome", "bentopick - the grid", "chrome.exe").is_some());
    }

    #[test]
    fn punctuation_in_the_query_is_matched_literally() {
        assert!(hit("design.md", "DESIGN.md - bentopick"));
        assert!(hit(r"r:\dev", r"R:\dev\bentopick"));
    }
}
