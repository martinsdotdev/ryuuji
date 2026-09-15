//! `EP01`, `E12`, `第10話`: an episode prefix glued to its number. The prefix
//! is used up and the number is read through the word shapes, or whole.

use crate::element::ElementKind;
use crate::engine::{RuleName, Verdict, WordRule};
use crate::keyword::KeywordTable;
use crate::numbering::Extent;
use crate::reading::{Certainty, Reading};
use crate::rules::take_number_from;
use crate::token::Tape;

pub(crate) const RULE: WordRule = WordRule {
    name: RuleName::EpisodeInWord,
    certainty: Certainty::Stated,
    read,
};

fn read(tape: &Tape, reading: &Reading, at: usize) -> Verdict {
    let text = &tape.tokens[at].text;
    let Some(digit_pos) = text.find(|c: char| c.is_ascii_digit()) else {
        return Verdict::nothing();
    };
    if digit_pos == 0
        || KeywordTable::builtin()
            .find(
                ElementKind::EpisodePrefix,
                &text[..digit_pos].to_uppercase(),
            )
            .is_none()
    {
        return Verdict::nothing();
    }
    take_number_from(
        Verdict::nothing().spend_part(ElementKind::EpisodePrefix, at, 0..digit_pos),
        reading,
        at,
        text,
        digit_pos,
        Extent::Episode,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::probe;

    #[test]
    fn the_digits_after_the_prefix_are_the_episode() {
        assert_eq!(
            probe(RuleName::EpisodeInWord, "[Group] Show EP01 [720p].mkv"),
            [(ElementKind::EpisodeNumber, "01".to_owned())]
        );
        assert_eq!(
            probe(RuleName::EpisodeInWord, "Yuru Yuri 第10話.mkv"),
            [(ElementKind::EpisodeNumber, "10".to_owned())]
        );
    }

    #[test]
    fn a_word_that_is_not_a_prefix_reads_nothing() {
        assert!(probe(RuleName::EpisodeInWord, "[Group] Show X01 [720p].mkv").is_empty());
    }
}
