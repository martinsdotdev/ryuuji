//! `Vol.3`, `V2`: a volume prefix glued to its number. The prefix is used up
//! and the number is read through the volume shapes, or whole.

use crate::element::ElementKind;
use crate::engine::{RuleName, Verdict, WordRule};
use crate::keyword::KeywordTable;
use crate::numbering::Extent;
use crate::reading::{Certainty, Reading};
use crate::rules::take_number_from;
use crate::token::Tape;

pub(crate) const RULE: WordRule = WordRule {
    name: RuleName::VolumeInWord,
    certainty: Certainty::Stated,
    read,
};

fn read(tape: &Tape, _reading: &Reading, at: usize) -> Verdict {
    let text = &tape.tokens[at].text;
    let Some(digit_pos) = text.find(|c: char| c.is_ascii_digit()) else {
        return Verdict::nothing();
    };
    if digit_pos == 0
        || KeywordTable::builtin()
            .find(ElementKind::VolumePrefix, &text[..digit_pos].to_uppercase())
            .is_none()
    {
        return Verdict::nothing();
    }
    take_number_from(
        Verdict::nothing().spend_part(ElementKind::VolumePrefix, at, 0..digit_pos),
        at,
        text,
        digit_pos,
        Extent::Volume,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::probe;

    #[test]
    fn the_digits_after_the_prefix_are_the_volume() {
        assert_eq!(
            probe(RuleName::VolumeInWord, "[Group] Show Vol.3 [720p].mkv"),
            [(ElementKind::VolumeNumber, "3".to_owned())]
        );
    }

    #[test]
    fn the_episode_is_still_read_after_a_volume() {
        let reading = crate::parse(
            "[Group] Show Vol.3 - 05 [720p].mkv",
            &crate::Options::default(),
        );
        assert_eq!(reading.elements().get(ElementKind::VolumeNumber), Some("3"));
        assert_eq!(
            reading.elements().get(ElementKind::EpisodeNumber),
            Some("05")
        );
    }
}
