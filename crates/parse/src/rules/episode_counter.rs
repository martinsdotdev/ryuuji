//! `12話`: a digit run closed by the Japanese episode counter.

use crate::engine::{RuleName, Verdict, WordRule};
use crate::numbering;
use crate::reading::{Certainty, Reading};
use crate::rules::take_shape;
use crate::token::Tape;

pub(crate) const RULE: WordRule = WordRule {
    name: RuleName::EpisodeCounter,
    certainty: Certainty::Stated,
    read,
};

fn read(tape: &Tape, reading: &Reading, at: usize) -> Verdict {
    take_shape(tape, reading, at, numbering::counter)
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
