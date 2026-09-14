//! `#12`, `#01-02v2`: a number sign before the episode.

use crate::engine::{RuleName, Verdict, WordRule};
use crate::numbering::{self, Extent};
use crate::reading::{Certainty, Reading};
use crate::rules::take_pieces;
use crate::token::Tape;

pub(crate) const RULE: WordRule = WordRule {
    name: RuleName::EpisodeSign,
    certainty: Certainty::Stated,
    read,
};

fn read(tape: &Tape, _reading: &Reading, at: usize) -> Verdict {
    match numbering::read_with(&tape.tokens[at].text, numbering::number_sign) {
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
    fn the_digits_after_the_sign_are_the_episode() {
        assert_eq!(
            probe(RuleName::EpisodeSign, "[Group] Show #12 [720p].mkv"),
            [(ElementKind::EpisodeNumber, "12".to_owned())]
        );
    }

    #[test]
    fn a_plain_number_is_not_this_shape() {
        assert!(probe(RuleName::EpisodeSign, "[Group] Show 01 [720p].mkv").is_empty());
    }
}
