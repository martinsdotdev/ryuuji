//! Terms the tokenizer cut out whole (`H.264`, `Dual Audio`, `10 bit`) are
//! read first, ahead of everything the table matches, because the order of
//! repeated kinds is part of what the fixtures pin.

use crate::engine::{Rule, RuleName, Verdict};
use crate::reading::{Certainty, Reading};
use crate::token::Tape;

pub(crate) const RULE: Rule = Rule {
    name: RuleName::Preidentified,
    settles: None,
    gate: |_| true,
    certainty: Certainty::Stated,
    read,
};

fn read(tape: &Tape, _reading: &Reading) -> Verdict {
    let mut verdict = Verdict::nothing();
    for (at, token) in tape.iter() {
        if let Some(kind) = token.term {
            verdict = verdict.take(kind, at);
        }
    }
    verdict
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::ElementKind;
    use crate::engine::probe;

    #[test]
    fn every_term_is_read_in_tape_order() {
        assert_eq!(
            probe(
                RuleName::Preidentified,
                "[Coalgirls]_Spirited_Away_(1080p_FLAC)_Dual Audio.mkv"
            ),
            [
                (ElementKind::VideoResolution, "1080p".to_owned()),
                (ElementKind::AudioTerm, "Dual Audio".to_owned()),
            ]
        );
    }

    #[test]
    fn a_name_without_terms_reads_nothing() {
        assert!(probe(RuleName::Preidentified, "Toradora! 01.mkv").is_empty());
    }
}
