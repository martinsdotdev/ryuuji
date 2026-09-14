//! What is left outside the brackets once an episode is read is the
//! episode title, unless it is nothing but a dash and a word.

use crate::element::ElementKind;
use crate::engine::{Rule, RuleName, Verdict};
use crate::reading::{Certainty, Reading};
use crate::string;
use crate::token::{Delimiters, Tape};

pub(crate) const RULE: Rule = Rule {
    name: RuleName::EpisodeTitle,
    settles: Some(ElementKind::EpisodeTitle),
    gate: |options| options.parse_episode_title,
    certainty: Certainty::Shaped,
    read,
};

fn read(tape: &Tape, reading: &Reading) -> Verdict {
    if !reading.elements().contains(ElementKind::EpisodeNumber) {
        return Verdict::nothing();
    }
    let Some(run) = tape
        .free_runs(false)
        .find(|run| run.len() > 2 || !string::is_dash(&tape.tokens[run.start].text))
    else {
        return Verdict::nothing();
    };
    Verdict::nothing().take_run(ElementKind::EpisodeTitle, run, Delimiters::Folded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::probe;

    #[test]
    fn the_run_after_the_episode_is_the_episode_title() {
        assert_eq!(
            probe(RuleName::EpisodeTitle, "[Group] Show - 03 - Snow Falls [720p].mkv"),
            [(ElementKind::EpisodeTitle, "Snow Falls".to_owned())]
        );
    }

    #[test]
    fn without_an_episode_there_is_no_episode_title() {
        assert!(probe(RuleName::EpisodeTitle, "[Group] Show - Snow Falls [720p].mkv").is_empty());
    }
}
