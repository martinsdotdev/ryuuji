//! `8 & 10`, `01 of 24`: a number joined to another. With `&` both are
//! episodes; with `of` only the first is, and the count is used up.

use crate::element::ElementKind;
use crate::engine::{RuleName, Verdict, WordRule};
use crate::reading::{Certainty, Reading};
use crate::token::{self, Tape};

pub(crate) const RULE: WordRule = WordRule {
    name: RuleName::EpisodePair,
    certainty: Certainty::Stated,
    read,
};

fn read(tape: &Tape, _reading: &Reading, at: usize) -> Verdict {
    if !tape.tokens[at]
        .text
        .starts_with(|c: char| c.is_ascii_digit())
    {
        return Verdict::nothing();
    }
    let Some(separator) = tape.next(at, token::is_not_delimiter) else {
        return Verdict::nothing();
    };
    let joiner = &tape.tokens[separator].text;
    let both = if joiner == "&" {
        true
    } else if joiner.eq_ignore_ascii_case("of") {
        false
    } else {
        return Verdict::nothing();
    };
    let Some(other) = tape.next(separator, token::is_not_delimiter) else {
        return Verdict::nothing();
    };
    if !tape.tokens[other].numeric {
        return Verdict::nothing();
    }
    let verdict = Verdict::nothing().take(ElementKind::EpisodeNumber, at);
    let verdict = if both {
        verdict.take(ElementKind::EpisodeNumber, other)
    } else {
        verdict.spend(ElementKind::EpisodeNumber, other)
    };
    verdict.spend(ElementKind::EpisodeNumber, separator)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::probe;

    #[test]
    fn an_ampersand_joins_two_episodes() {
        assert_eq!(
            probe(RuleName::EpisodePair, "[Group] Show 8 & 10 [720p].mkv"),
            [
                (ElementKind::EpisodeNumber, "8".to_owned()),
                (ElementKind::EpisodeNumber, "10".to_owned()),
            ]
        );
    }

    #[test]
    fn of_keeps_only_the_first_number() {
        assert_eq!(
            probe(RuleName::EpisodePair, "[Group] Show 01 of 24 [720p].mkv"),
            [(ElementKind::EpisodeNumber, "01".to_owned())]
        );
    }

    #[test]
    fn a_word_joined_to_a_word_reads_nothing() {
        assert!(probe(RuleName::EpisodePair, "[Group] Tom & Jerry 01.mkv").is_empty());
    }
}
