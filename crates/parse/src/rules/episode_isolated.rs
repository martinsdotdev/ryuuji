//! `[12]`: a number alone between brackets, when nothing else in the name
//! reads as the episode.

use crate::element::ElementKind;
use crate::engine::{Rule, RuleName, Verdict};
use crate::numbering::{EPISODE_NUMBER_MAX, leading_value};
use crate::reading::{Certainty, Reading};
use crate::token::Tape;

pub(crate) const RULE: Rule = Rule {
    name: RuleName::EpisodeIsolated,
    settles: Some(ElementKind::EpisodeNumber),
    gate: |options| options.parse_episode_number,
    certainty: Certainty::Shaped,
    read,
};

fn read(tape: &Tape, _reading: &Reading) -> Verdict {
    for (at, token) in tape.free() {
        if !token.numeric
            || !token.enclosed
            || !tape.isolated(at)
            || leading_value(&token.text) > EPISODE_NUMBER_MAX
        {
            continue;
        }
        return Verdict::nothing().take(ElementKind::EpisodeNumber, at);
    }
    Verdict::nothing()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::probe;

    #[test]
    fn a_number_alone_in_brackets_is_the_episode() {
        assert_eq!(
            probe(RuleName::EpisodeIsolated, "[Group] Show [12] [720p].mkv"),
            [(ElementKind::EpisodeNumber, "12".to_owned())]
        );
    }

    #[test]
    fn a_number_outside_brackets_is_left_to_a_later_rule() {
        assert!(probe(RuleName::EpisodeIsolated, "[Group] Show 12 [720p].mkv").is_empty());
    }
}
