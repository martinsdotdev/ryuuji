//! `OVA1`: an anime type glued to its number. The type stays a title word
//! when the table says it is not identifiable; only the number is used up.

use crate::engine::{RuleName, Verdict, WordRule};
use crate::numbering::{self, Extent};
use crate::reading::{Certainty, Reading};
use crate::rules::take_pieces;
use crate::token::Tape;

pub(crate) const RULE: WordRule = WordRule {
    name: RuleName::EpisodeType,
    certainty: Certainty::Stated,
    read,
};

fn read(tape: &Tape, _reading: &Reading, at: usize) -> Verdict {
    match numbering::read_with(&tape.tokens[at].text, |word| {
        numbering::type_and_episode(word)
    }) {
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
    fn the_type_and_the_number_are_read() {
        assert_eq!(
            probe(RuleName::EpisodeType, "[Group] Show OVA1 [720p].mkv"),
            [
                (ElementKind::AnimeType, "OVA".to_owned()),
                (ElementKind::EpisodeNumber, "1".to_owned())
            ]
        );
    }

    #[test]
    fn a_plain_number_is_not_this_shape() {
        assert!(probe(RuleName::EpisodeType, "[Group] Show 01 [720p].mkv").is_empty());
    }
}
