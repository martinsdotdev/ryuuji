//! `07.5`, `04.1`: a half or a leading-zero fraction is an episode.

use crate::engine::{RuleName, Verdict, WordRule};
use crate::numbering;
use crate::reading::{Certainty, Reading};
use crate::rules::take_shape;
use crate::token::Tape;

pub(crate) const RULE: WordRule = WordRule {
    name: RuleName::EpisodeFraction,
    certainty: Certainty::Stated,
    read,
};

fn read(tape: &Tape, reading: &Reading, at: usize) -> Verdict {
    take_shape(tape, reading, at, numbering::fraction)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::ElementKind;
    use crate::engine::probe;

    #[test]
    fn a_half_episode_is_read_whole() {
        assert_eq!(
            probe(RuleName::EpisodeFraction, "[Group] Show 07.5 [720p].mkv"),
            [(ElementKind::EpisodeNumber, "07.5".to_owned())]
        );
    }

    #[test]
    fn a_plain_number_is_not_this_shape() {
        assert!(probe(RuleName::EpisodeFraction, "[Group] Show 01 [720p].mkv").is_empty());
    }
}
