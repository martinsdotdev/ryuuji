//! The last number outside the brackets is the episode when nothing in the
//! name marks one. A guess: `Byousoku 5 Centimeter` reads `5` this way, and
//! `5` is also a word of the title, so the doubt is recorded.
//!
//! The episode comes after the title, so the first token outside brackets
//! is not it, unless it is the only one and a title waits inside a bracket
//! group (`[Group][Title] 02 [720p]`). A number after `Movie` or `Part`
//! counts the film, not the episode.

use crate::element::ElementKind;
use crate::engine::{Rule, RuleName, Verdict};
use crate::numbering::{EPISODE_NUMBER_MAX, leading_value};
use crate::reading::{Certainty, Reading, Sense};
use crate::rules::enclosed_title_begin;
use crate::token::{self, Tape};

pub(crate) const RULE: Rule = Rule {
    name: RuleName::EpisodeLast,
    settles: Some(ElementKind::EpisodeNumber),
    gate: |options| options.parse_episode_number,
    certainty: Certainty::Guessed,
    read,
};

fn read(tape: &Tape, _reading: &Reading) -> Verdict {
    let numeric: Vec<usize> = tape
        .free()
        .filter(|(_, token)| token.numeric)
        .map(|(at, _)| at)
        .collect();
    for &at in numeric.iter().rev() {
        if at == 0 || tape.tokens[at].enclosed {
            continue;
        }
        let first_outside = tape.tokens[..at]
            .iter()
            .all(|token| token.enclosed || token.is_delimiter());
        let only_outside = !tape.tokens[at + 1..]
            .iter()
            .any(|token| !token.enclosed && token.is_free());
        if first_outside && !(only_outside && enclosed_title_begin(tape).is_some()) {
            continue;
        }
        if let Some(prev) = tape.prev(at, token::is_not_delimiter)
            && tape.tokens[prev].is_free()
            && (tape.tokens[prev].text.eq_ignore_ascii_case("movie")
                || tape.tokens[prev].text.eq_ignore_ascii_case("part"))
        {
            continue;
        }
        if leading_value(&tape.tokens[at].text) > EPISODE_NUMBER_MAX {
            continue;
        }
        return Verdict::nothing()
            .take(ElementKind::EpisodeNumber, at)
            .instead(
                at,
                Sense {
                    kind: None,
                    rule: RuleName::Title,
                },
            );
    }
    Verdict::nothing()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::probe;
    use crate::options::Options;

    #[test]
    fn the_last_number_outside_the_brackets_is_the_episode() {
        assert_eq!(
            probe(RuleName::EpisodeLast, "[Group] Show 2 Title 12 [720p].mkv"),
            [(ElementKind::EpisodeNumber, "12".to_owned())]
        );
    }

    #[test]
    fn the_first_word_outside_the_brackets_is_the_title_not_the_episode() {
        assert!(probe(RuleName::EpisodeLast, "[Group] 12 [720p].mkv").is_empty());
        assert_eq!(
            probe(RuleName::EpisodeLast, "[Group][Show] 12 [720p].mkv"),
            [(ElementKind::EpisodeNumber, "12".to_owned())]
        );
    }

    #[test]
    fn a_number_after_movie_or_part_is_not() {
        assert!(probe(RuleName::EpisodeLast, "[Group] Show Movie 2 [720p].mkv").is_empty());
    }

    #[test]
    fn the_guess_records_the_title_word_it_displaced() {
        let reading = crate::parse("Byousoku 5 Centimeter [1080p].mkv", &Options::default());
        let alternative = &reading.alternatives()[0];
        assert_eq!(alternative.text, "5");
        assert_eq!(alternative.taken.kind, Some(ElementKind::EpisodeNumber));
        assert_eq!(alternative.passed.kind, None);
        assert_eq!(
            reading.episodes().map(|e| e.certainty),
            Some(Certainty::Guessed)
        );
    }
}
