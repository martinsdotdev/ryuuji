//! An unidentifiable term (`Ita`, `END`, `Opus`) leaves its word to the
//! title, and that is how the two are told apart: a term that turned out
//! to be a word of the anime title was never an element.
//!
//! Anime types are exempt: the corpus keeps `Movie` wherever it sits (`The
//! New Movie Q`, `Movie Part 1`) yet drops `Special` when a word follows
//! it (`Special A`), and nothing but the word itself separates the two.
//! They keep only the episode-title check.

use crate::element::ElementKind;
use crate::engine::{Rule, RuleName, Verdict};
use crate::keyword::KeywordTable;
use crate::reading::{Certainty, Reading};
use crate::token::Tape;

pub(crate) const RULE: Rule = Rule {
    name: RuleName::TermInAnimeTitle,
    settles: None,
    gate: |_| true,
    certainty: Certainty::Stated,
    read,
};

fn read(_tape: &Tape, reading: &Reading) -> Verdict {
    let Some(title) = reading.elements().get(ElementKind::AnimeTitle) else {
        return Verdict::nothing();
    };
    let mut verdict = Verdict::nothing();
    for (kind, value) in unidentifiable(reading) {
        if kind != ElementKind::AnimeType && has_word(title, value) {
            verdict = verdict.retract(kind, value, None);
        }
    }
    verdict
}

/// Every fact whose value the table lists as a term that may not be
/// identified on its own.
pub(super) fn unidentifiable(reading: &Reading) -> impl Iterator<Item = (ElementKind, &str)> {
    let table = KeywordTable::builtin();
    reading.elements().iter().filter(move |(kind, value)| {
        table
            .find(*kind, &value.to_uppercase())
            .is_some_and(|keyword| !keyword.identifiable)
    })
}

/// Whether `word` appears whole in `text`, case-insensitively, between
/// non-alphanumeric characters or the ends.
pub(super) fn has_word(text: &str, word: &str) -> bool {
    text.split(|c: char| !c.is_alphanumeric())
        .any(|candidate| candidate.eq_ignore_ascii_case(word))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::Options;

    #[test]
    fn a_word_matches_whole_and_case_insensitively() {
        assert!(has_word("Bokura Ga Ita", "ITA"));
        assert!(has_word("The End of Evangelion", "end"));
        assert!(!has_word("Weekend", "END"));
        assert!(!has_word("Specials", "Special"));
    }

    #[test]
    fn a_term_the_title_kept_is_taken_back_and_recorded() {
        let reading = crate::parse("[Group] Bokura Ga Ita - 03.mkv", &Options::default());
        assert_eq!(reading.elements().get(ElementKind::Language), None);
        assert_eq!(reading.title().map(|t| t.value), Some("Bokura Ga Ita"));
        let alternative = &reading.alternatives()[0];
        assert_eq!(alternative.text, "Ita");
        assert_eq!(alternative.passed.kind, Some(ElementKind::Language));
        assert_eq!(alternative.taken.rule, RuleName::TermInAnimeTitle);
    }

    #[test]
    fn an_anime_type_stays_whatever_the_title_keeps() {
        let reading = crate::parse("[Group] The New Movie Q - 03.mkv", &Options::default());
        assert_eq!(
            reading.elements().get(ElementKind::AnimeType),
            Some("Movie")
        );
    }
}
