//! `4a`: an episode and a part letter.

use crate::engine::{RuleName, Verdict, WordRule};
use crate::numbering;
use crate::reading::{Certainty, Reading};
use crate::rules::take_shape;
use crate::token::Tape;

pub(crate) const RULE: WordRule = WordRule {
    name: RuleName::EpisodePartial,
    certainty: Certainty::Stated,
    read,
};

fn read(tape: &Tape, reading: &Reading, at: usize) -> Verdict {
    take_shape(tape, reading, at, numbering::partial)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::ElementKind;
    use crate::engine::probe;

    #[test]
    fn a_part_letter_stays_on_the_episode() {
        assert_eq!(
            probe(RuleName::EpisodePartial, "[Group] Show 4a [720p].mkv"),
            [(ElementKind::EpisodeNumber, "4a".to_owned())]
        );
    }

    #[test]
    fn a_plain_number_is_not_this_shape() {
        assert!(probe(RuleName::EpisodePartial, "[Group] Show 01 [720p].mkv").is_empty());
    }
}
