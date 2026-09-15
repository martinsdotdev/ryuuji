//! `05v2`: an episode and its release version in one word.

use crate::engine::{RuleName, Verdict, WordRule};
use crate::numbering::{self, Extent};
use crate::reading::{Certainty, Reading};
use crate::rules::take_shape;
use crate::token::Tape;

pub(crate) const RULE: WordRule = WordRule {
    name: RuleName::EpisodeVersion,
    certainty: Certainty::Stated,
    read,
};

fn read(tape: &Tape, reading: &Reading, at: usize) -> Verdict {
    take_shape(tape, reading, at, |word| {
        numbering::version_suffix(word, Extent::Episode)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::ElementKind;
    use crate::engine::probe;

    #[test]
    fn the_number_and_the_version_are_read() {
        assert_eq!(
            probe(RuleName::EpisodeVersion, "[Group] Show 05v2 [720p].mkv"),
            [
                (ElementKind::EpisodeNumber, "05".to_owned()),
                (ElementKind::ReleaseVersion, "2".to_owned())
            ]
        );
    }

    #[test]
    fn a_plain_number_is_not_this_shape() {
        assert!(probe(RuleName::EpisodeVersion, "[Group] Show 01 [720p].mkv").is_empty());
    }
}
