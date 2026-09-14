//! `Title - 08`, or `08 - Episode Title` when the number opens the name: a
//! number set off by a dash. The dash is used up with it.

use crate::element::ElementKind;
use crate::engine::{Rule, RuleName, Verdict};
use crate::numbering::{EPISODE_NUMBER_MAX, leading_value};
use crate::reading::{Certainty, Reading};
use crate::string;
use crate::token::{self, Tape};

pub(crate) const RULE: Rule = Rule {
    name: RuleName::EpisodeSeparated,
    settles: Some(ElementKind::EpisodeNumber),
    gate: |options| options.parse_episode_number,
    certainty: Certainty::Shaped,
    read,
};

fn read(tape: &Tape, _reading: &Reading) -> Verdict {
    let dash = |neighbour: Option<usize>| {
        neighbour.filter(|&neighbour| {
            tape.tokens[neighbour].is_free() && string::is_dash(&tape.tokens[neighbour].text)
        })
    };
    for (at, token) in tape.free() {
        if !token.numeric {
            continue;
        }
        let prev = tape.prev(at, token::is_not_delimiter);
        let separator = match dash(prev) {
            Some(prev) => prev,
            None if prev.is_none() => {
                let Some(next) = dash(tape.next(at, token::is_not_delimiter)) else {
                    continue;
                };
                next
            }
            None => continue,
        };
        if leading_value(&token.text) > EPISODE_NUMBER_MAX {
            continue;
        }
        return Verdict::nothing()
            .take(ElementKind::EpisodeNumber, at)
            .spend(ElementKind::EpisodeNumber, separator);
    }
    Verdict::nothing()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::probe;

    #[test]
    fn a_number_after_a_dash_is_the_episode() {
        assert_eq!(
            probe(RuleName::EpisodeSeparated, "[Group] Show - 08 [720p].mkv"),
            [(ElementKind::EpisodeNumber, "08".to_owned())]
        );
    }

    #[test]
    fn a_leading_number_before_a_dash_is_the_episode() {
        assert_eq!(
            probe(RuleName::EpisodeSeparated, "08 - The Hidden Village.mkv"),
            [(ElementKind::EpisodeNumber, "08".to_owned())]
        );
    }

    #[test]
    fn a_number_past_the_episode_bound_is_not() {
        assert!(probe(RuleName::EpisodeSeparated, "[Group] Show - 2009 [720p].mkv").is_empty());
    }
}
