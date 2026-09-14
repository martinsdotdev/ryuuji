//! `02 (100)`: a plain number followed by one alone in brackets is one
//! episode under two numbering schemes. The smaller is the episode and the
//! larger the alternative, whichever comes first.

use crate::element::ElementKind;
use crate::engine::{Rule, RuleName, Verdict};
use crate::numbering::{EPISODE_NUMBER_MAX, leading_value};
use crate::reading::{Certainty, Reading};
use crate::token::{self, Tape};

pub(crate) const RULE: Rule = Rule {
    name: RuleName::EpisodeEquivalent,
    settles: Some(ElementKind::EpisodeNumber),
    gate: |options| options.parse_episode_number,
    certainty: Certainty::Shaped,
    read,
};

fn read(tape: &Tape, _reading: &Reading) -> Verdict {
    let within_bound = |at: usize| leading_value(&tape.tokens[at].text) <= EPISODE_NUMBER_MAX;
    for (at, token) in tape.free() {
        if !token.numeric || tape.isolated(at) || !within_bound(at) {
            continue;
        }
        let Some(bracket) = tape.next(at, token::is_not_delimiter) else {
            continue;
        };
        if !tape.tokens[bracket].is_bracket() {
            continue;
        }
        let Some(other) = tape.next(bracket, |token| {
            token.enclosed && token::is_not_delimiter(token)
        }) else {
            continue;
        };
        let other_token = &tape.tokens[other];
        if !other_token.is_free()
            || !other_token.numeric
            || !tape.isolated(other)
            || !within_bound(other)
        {
            continue;
        }
        let (episode, alt) = if leading_value(&other_token.text) < leading_value(&token.text) {
            (other, at)
        } else {
            (at, other)
        };
        return Verdict::nothing()
            .take(ElementKind::EpisodeNumber, episode)
            .take(ElementKind::EpisodeNumberAlt, alt);
    }
    Verdict::nothing()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::probe;

    #[test]
    fn the_smaller_number_is_the_episode() {
        assert_eq!(
            probe(
                RuleName::EpisodeEquivalent,
                "[Group] Show 02 (100) [720p].mkv"
            ),
            [
                (ElementKind::EpisodeNumber, "02".to_owned()),
                (ElementKind::EpisodeNumberAlt, "100".to_owned()),
            ]
        );
        assert_eq!(
            probe(
                RuleName::EpisodeEquivalent,
                "[Group] Show 100 (02) [720p].mkv"
            ),
            [
                (ElementKind::EpisodeNumber, "02".to_owned()),
                (ElementKind::EpisodeNumberAlt, "100".to_owned()),
            ]
        );
    }

    #[test]
    fn a_number_with_no_bracketed_twin_reads_nothing() {
        assert!(probe(RuleName::EpisodeEquivalent, "[Group] Show 02 [720p].mkv").is_empty());
    }
}
