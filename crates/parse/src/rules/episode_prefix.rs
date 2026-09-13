//! `Ep. 08`, `Episode 12`, `Folge 3`: an episode word followed by a number.
//! The word is used up and the number is read through the word shapes, so
//! `Ep. 01v2` reads a version too. An episode read this way is provisional:
//! a later, different number is the same episode under another scheme, and
//! the engine settles which is which.

use crate::element::ElementKind;
use crate::engine::{Rule, RuleName, Verdict};
use crate::keyword::KeywordTable;
use crate::numbering::Extent;
use crate::reading::{Certainty, Reading};
use crate::rules::take_number;
use crate::string;
use crate::token::{self, Tape};

pub(crate) const RULE: Rule = Rule {
    name: RuleName::EpisodePrefix,
    settles: None,
    gate: |_| true,
    certainty: Certainty::Stated,
    read,
};

fn read(tape: &Tape, _reading: &Reading) -> Verdict {
    let table = KeywordTable::builtin();
    let mut verdict = Verdict::nothing();
    let mut read_one = false;
    for (at, token) in tape.free() {
        let word = string::trim_dashes_and_spaces(&token.text);
        if word.is_empty() || string::is_numeric(word) {
            continue;
        }
        if table
            .find_searchable(&word.to_uppercase())
            .is_none_or(|keyword| keyword.kind != ElementKind::EpisodePrefix || !keyword.valid)
        {
            continue;
        }
        let Some(next) = tape.next(at, token::is_not_delimiter) else {
            continue;
        };
        let number = &tape.tokens[next];
        if !number.is_free() || !number.text.starts_with(|c: char| c.is_ascii_digit()) {
            continue;
        }
        verdict = take_number(
            verdict.spend(ElementKind::EpisodePrefix, at),
            next,
            &number.text,
            Extent::Episode,
        );
        read_one = true;
    }
    if read_one { verdict.provisional() } else { verdict }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::probe;

    #[test]
    fn the_number_after_the_word_is_the_episode() {
        assert_eq!(
            probe(RuleName::EpisodePrefix, "[Group] Show Ep. 08 [720p].mkv"),
            [(ElementKind::EpisodeNumber, "08".to_owned())]
        );
    }

    #[test]
    fn the_number_is_read_through_the_word_shapes() {
        assert_eq!(
            probe(RuleName::EpisodePrefix, "[Group] Show Episode 01v2.mkv"),
            [
                (ElementKind::EpisodeNumber, "01".to_owned()),
                (ElementKind::ReleaseVersion, "2".to_owned()),
            ]
        );
    }

    #[test]
    fn a_word_without_a_number_after_it_reads_nothing() {
        assert!(probe(RuleName::EpisodePrefix, "[Group] Show Episode Title.mkv").is_empty());
    }

    #[test]
    fn a_second_number_settles_as_another_scheme() {
        let reading = crate::parse("Show Ep. 08 - 05v2.mkv", &crate::Options::default());
        assert_eq!(reading.elements().get(ElementKind::EpisodeNumber), Some("05"));
        assert_eq!(
            reading.elements().get(ElementKind::EpisodeNumberAlt),
            Some("08")
        );
    }
}
