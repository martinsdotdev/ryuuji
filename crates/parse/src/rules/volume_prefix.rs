//! `Vol. 3`, `Volume 12`: a volume word followed by a number. The word is
//! used up and the number is read through the volume shapes.

use crate::element::ElementKind;
use crate::engine::{Rule, RuleName, Verdict};
use crate::keyword::KeywordTable;
use crate::numbering::Extent;
use crate::reading::{Certainty, Reading};
use crate::rules::take_number;
use crate::string;
use crate::token::{self, Tape};

pub(crate) const RULE: Rule = Rule {
    name: RuleName::VolumePrefix,
    settles: None,
    gate: |_| true,
    certainty: Certainty::Stated,
    read,
};

fn read(tape: &Tape, _reading: &Reading) -> Verdict {
    let table = KeywordTable::builtin();
    let mut verdict = Verdict::nothing();
    for (at, token) in tape.free() {
        let word = string::trim_dashes_and_spaces(&token.text);
        if word.is_empty() || string::is_numeric(word) {
            continue;
        }
        if table
            .find_searchable(&word.to_uppercase())
            .is_none_or(|keyword| keyword.kind != ElementKind::VolumePrefix)
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
            verdict.spend(ElementKind::VolumePrefix, at),
            next,
            &number.text,
            Extent::Volume,
        );
    }
    verdict
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
