//! `01-02`, `01+02`, `01v2-02v3`: a batch of episodes in one word.

use crate::engine::{RuleName, Verdict, WordRule};
use crate::numbering::{self, Extent};
use crate::reading::{Certainty, Reading};
use crate::rules::take_shape;
use crate::token::Tape;

pub(crate) const RULE: WordRule = WordRule {
    name: RuleName::EpisodeRange,
    certainty: Certainty::Stated,
    read,
};

fn read(tape: &Tape, reading: &Reading, at: usize) -> Verdict {
    take_shape(tape, reading, at, |word| {
        numbering::range(word, Extent::Episode)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::ElementKind;
    use crate::engine::probe;

    #[test]
    fn both_ends_of_the_batch_are_episodes() {
        assert_eq!(
            probe(RuleName::EpisodeRange, "[Group] Show 01-02 [720p].mkv"),
            [
                (ElementKind::EpisodeNumber, "01".to_owned()),
                (ElementKind::EpisodeNumber, "02".to_owned())
            ]
        );
    }

    #[test]
    fn a_plain_number_is_not_this_shape() {
        assert!(probe(RuleName::EpisodeRange, "[Group] Show 01 [720p].mkv").is_empty());
    }
}
