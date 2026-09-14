//! `S01E06`, `2x05`: a season and an episode in one word.

use crate::engine::{RuleName, Verdict, WordRule};
use crate::numbering::{self, Extent};
use crate::reading::{Certainty, Reading};
use crate::rules::take_pieces;
use crate::token::Tape;

pub(crate) const RULE: WordRule = WordRule {
    name: RuleName::SeasonEpisode,
    certainty: Certainty::Stated,
    read,
};

fn read(tape: &Tape, _reading: &Reading, at: usize) -> Verdict {
    match numbering::read_with(&tape.tokens[at].text, |word| {
        numbering::season_and_episode(word)
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
    fn the_season_and_the_episode_are_read() {
        assert_eq!(
            probe(RuleName::SeasonEpisode, "[Group] Show S02E04v2 [720p].mkv"),
            [
                (ElementKind::AnimeSeason, "02".to_owned()),
                (ElementKind::EpisodeNumber, "04".to_owned()),
                (ElementKind::ReleaseVersion, "2".to_owned())
            ]
        );
    }

    #[test]
    fn a_plain_number_is_not_this_shape() {
        assert!(probe(RuleName::SeasonEpisode, "[Group] Show 01 [720p].mkv").is_empty());
    }
}
