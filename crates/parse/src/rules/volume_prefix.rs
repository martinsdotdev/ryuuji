//! `Vol. 3`, `Volume 12`: a volume word followed by a number. The word is
//! used up and the number is read through the volume shapes.

use crate::element::ElementKind;
use crate::engine::{RuleName, Verdict, WordRule};
use crate::numbering::Extent;
use crate::reading::{Certainty, Reading};
use crate::rules::{keyword_at, take_number};
use crate::token::{self, Tape};

pub(crate) const RULE: WordRule = WordRule {
    name: RuleName::VolumePrefix,
    certainty: Certainty::Stated,
    read,
};

fn read(tape: &Tape, reading: &Reading, at: usize) -> Verdict {
    if keyword_at(tape, at).is_none_or(|(_, keyword)| keyword.kind != ElementKind::VolumePrefix) {
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
        Verdict::nothing().spend(ElementKind::VolumePrefix, at),
        reading,
        next,
        &number.text,
        Extent::Volume,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::probe;

    #[test]
    fn the_number_after_the_word_is_the_volume() {
        assert_eq!(
            probe(RuleName::VolumePrefix, "[Group] Show Vol. 3 (BD 1080p).mkv"),
            [(ElementKind::VolumeNumber, "3".to_owned())]
        );
    }

    #[test]
    fn a_volume_range_reads_both_ends() {
        assert_eq!(
            probe(RuleName::VolumePrefix, "[Group] Show Vol.1-3 (BD).mkv"),
            [
                (ElementKind::VolumeNumber, "1".to_owned()),
                (ElementKind::VolumeNumber, "3".to_owned()),
            ]
        );
    }
}
