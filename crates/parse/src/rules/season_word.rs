//! `2nd Season`, `Season 2`, `Saison 3`: a season word with an ordinal
//! before it or a number after it. The word is used up; the number is the
//! season. The ordinal is read whether or not an earlier word already took
//! it, as `Ep 2nd Season` reads episode `2nd` and season 2.

use crate::element::ElementKind;
use crate::engine::{RuleName, Verdict, WordRule};
use crate::reading::{Certainty, Reading};
use crate::rules::keyword_at;
use crate::token::{self, Tape};

pub(crate) const RULE: WordRule = WordRule {
    name: RuleName::SeasonWord,
    certainty: Certainty::Stated,
    read,
};

fn read(tape: &Tape, _reading: &Reading, at: usize) -> Verdict {
    if keyword_at(tape, at)
        .is_none_or(|(_, keyword)| keyword.kind != ElementKind::AnimeSeasonPrefix)
    {
        return Verdict::nothing();
    }
    if let Some(prev) = tape.prev(at, token::is_not_delimiter)
        && let Some(number) = ordinal_number(&tape.tokens[prev].text)
    {
        let whole = 0..tape.tokens[prev].text.len();
        return Verdict::nothing()
            .take_part(ElementKind::AnimeSeason, prev, whole, number)
            .spend(ElementKind::AnimeSeasonPrefix, at);
    }
    if let Some(next) = tape.next(at, token::is_not_delimiter)
        && tape.tokens[next].numeric
    {
        return Verdict::nothing()
            .spend(ElementKind::AnimeSeasonPrefix, at)
            .take(ElementKind::AnimeSeason, next);
    }
    Verdict::nothing()
}

fn ordinal_number(word: &str) -> Option<&'static str> {
    match word.to_uppercase().as_str() {
        "1ST" | "FIRST" => Some("1"),
        "2ND" | "SECOND" => Some("2"),
        "3RD" | "THIRD" => Some("3"),
        "4TH" | "FOURTH" => Some("4"),
        "5TH" | "FIFTH" => Some("5"),
        "6TH" | "SIXTH" => Some("6"),
        "7TH" | "SEVENTH" => Some("7"),
        "8TH" | "EIGHTH" => Some("8"),
        "9TH" | "NINTH" => Some("9"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::probe;

    #[test]
    fn an_ordinal_before_the_word_is_the_season() {
        assert_eq!(
            probe(RuleName::SeasonWord, "[Group] Show 2nd Season - 03.mkv"),
            [(ElementKind::AnimeSeason, "2".to_owned())]
        );
    }

    #[test]
    fn a_number_after_the_word_is_the_season() {
        assert_eq!(
            probe(RuleName::SeasonWord, "[Group] Show Season 3 - 03.mkv"),
            [(ElementKind::AnimeSeason, "3".to_owned())]
        );
    }

    #[test]
    fn a_season_word_with_no_number_reads_nothing() {
        assert!(probe(RuleName::SeasonWord, "[Group] Show Season - 03.mkv").is_empty());
    }
}
