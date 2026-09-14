//! An episode title that is nothing but an unidentifiable term was never a
//! title; a term that is a word of the episode title was never an element.

use crate::element::ElementKind;
use crate::engine::{Rule, RuleName, Verdict};
use crate::reading::{Certainty, Reading};
use crate::rules::term_in_anime_title::{has_word, unidentifiable};
use crate::token::Tape;

pub(crate) const RULE: Rule = Rule {
    name: RuleName::TermIsEpisodeTitle,
    settles: None,
    gate: |_| true,
    certainty: Certainty::Stated,
    read,
};

fn read(_tape: &Tape, reading: &Reading) -> Verdict {
    let Some(title) = reading.elements().get(ElementKind::EpisodeTitle) else {
        return Verdict::nothing();
    };
    let mut verdict = Verdict::nothing();
    for (kind, value) in unidentifiable(reading) {
        if title.eq_ignore_ascii_case(value) {
            verdict = verdict.retract(ElementKind::EpisodeTitle, title);
        } else if has_word(title, value) {
            verdict = verdict.retract(kind, value);
        }
    }
    verdict
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::Options;

    #[test]
    fn an_episode_title_that_is_only_a_term_is_taken_back() {
        let reading = crate::parse("[Group] Show - 03 - END.mkv", &Options::default());
        assert_eq!(reading.elements().get(ElementKind::EpisodeTitle), None);
        assert_eq!(reading.elements().get(ElementKind::Other), Some("END"));
    }

    #[test]
    fn a_term_inside_the_episode_title_is_a_word_of_it() {
        let reading = crate::parse("[Group] Show - 03 - The END of It.mkv", &Options::default());
        assert_eq!(reading.elements().get(ElementKind::Other), None);
        assert_eq!(
            reading.elements().get(ElementKind::EpisodeTitle),
            Some("The END of It")
        );
    }
}
