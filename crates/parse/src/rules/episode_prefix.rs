//! `Ep. 08`, `Episode 12`, `Folge 3`: an episode word followed by a number.
//! The word is used up and the number is read through the word shapes, so
//! `Ep. 01v2` reads a version too. An episode read this way is provisional:
//! a different number read by a later step is the same episode under
//! another scheme, and the engine settles which is which.

use crate::element::ElementKind;
use crate::engine::{RuleName, Verdict, WordRule};
use crate::numbering::Extent;
use crate::reading::{Certainty, Reading};
use crate::rules::{keyword_at, take_number};
use crate::token::{self, Tape};

pub(crate) const RULE: WordRule = WordRule {
    name: RuleName::EpisodePrefix,
    certainty: Certainty::Stated,
    read,
};

fn read(tape: &Tape, _reading: &Reading, at: usize) -> Verdict {
    if keyword_at(tape, at)
        .is_none_or(|(_, keyword)| keyword.kind != ElementKind::EpisodePrefix || !keyword.valid)
    {
        return Verdict::nothing();
    }
    let Some(next) = tape.next(at, token::is_not_delimiter) else {
        return Verdict::nothing();
    };
    let number = &tape.tokens[next];
    if !number.is_free() || !number.text.starts_with(|c: char| c.is_ascii_digit()) {
        return Verdict::nothing();
    }
    take_number(
        Verdict::nothing().spend(ElementKind::EpisodePrefix, at),
        next,
        &number.text,
        Extent::Episode,
    )
    .provisional()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::probe;
    use crate::options::Options;

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
        let reading = crate::parse("Show Ep. 08 - 05v2.mkv", &Options::default());
        assert_eq!(
            reading.elements().get(ElementKind::EpisodeNumber),
            Some("05")
        );
        assert_eq!(
            reading.elements().get(ElementKind::EpisodeNumberAlt),
            Some("08")
        );
    }

    #[test]
    fn two_prefixed_episodes_are_a_batch() {
        let reading = crate::parse("Show Ep 01 Ep 02.mkv", &Options::default());
        assert_eq!(
            reading.elements().get_all(ElementKind::EpisodeNumber),
            ["01", "02"]
        );
    }
}
