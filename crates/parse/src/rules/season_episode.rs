//! `S01E06`, `2x05`: a season and an episode in one word.

use crate::engine::{RuleName, Verdict, WordRule};
use crate::numbering;
use crate::reading::{Certainty, Reading};
use crate::rules::take_shape;
use crate::token::Tape;

pub(crate) const RULE: WordRule = WordRule {
    name: RuleName::SeasonEpisode,
    certainty: Certainty::Stated,
    read,
};

fn read(tape: &Tape, reading: &Reading, at: usize) -> Verdict {
    take_shape(tape, reading, at, |word| {
        numbering::season_and_episode(word)
    })
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
