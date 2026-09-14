//! `12話`: a digit run closed by the Japanese episode counter.

use crate::engine::{RuleName, Verdict, WordRule};
use crate::numbering::{self, Extent};
use crate::reading::{Certainty, Reading};
use crate::rules::take_pieces;
use crate::token::Tape;

pub(crate) const RULE: WordRule = WordRule {
    name: RuleName::EpisodeCounter,
    certainty: Certainty::Stated,
    read,
};

fn read(tape: &Tape, _reading: &Reading, at: usize) -> Verdict {
    match numbering::read_with(&tape.tokens[at].text, numbering::counter) {
        Some(numbering) => take_pieces(Verdict::nothing(), at, numbering, Extent::Episode),
        None => Verdict::nothing(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::ElementKind;
    use crate::engine::probe;

    #[test]
    fn the_digits_before_the_counter_are_the_episode() {
        assert_eq!(
            probe(RuleName::EpisodeCounter, "Show 12話.mkv"),
            [(ElementKind::EpisodeNumber, "12".to_owned())]
        );
    }

    #[test]
    fn a_plain_number_is_not_this_shape() {
        assert!(probe(RuleName::EpisodeCounter, "[Group] Show 01 [720p].mkv").is_empty());
    }
}
