//! The anime title: the first run of free words outside the brackets, cut
//! before a trailing bracket group unless that group is parenthesized, so
//! `Anime (TV)` stays whole. When nothing free stands outside the brackets,
//! the second bracket group is the title, on the assumption that the first
//! is the release group.
//!
//! A name that leads with its episode and sets it off with a dash (`03 -
//! The Hidden Village`, `Ep. 07 - Snow Falls`) has no anime title: what
//! follows the dash is the episode title. Without the dash (`Episode 14
//! Ore no Imouto`) the rest is the anime title as usual.

use crate::element::ElementKind;
use crate::engine::{Rule, RuleName, Verdict};
use crate::reading::{Certainty, Reading};
use crate::rules::{enclosed_title_begin, episode_token};
use crate::string;
use crate::token::{self, Delimiters, Tape, Token};

pub(crate) const RULE: Rule = Rule {
    name: RuleName::Title,
    settles: Some(ElementKind::AnimeTitle),
    gate: |_| true,
    certainty: Certainty::Shaped,
    read,
};

fn read(tape: &Tape, reading: &Reading) -> Verdict {
    let unenclosed = tape
        .iter()
        .find(|(_, token)| !token.enclosed && token.is_free())
        .map(|(at, _)| at);
    let (begin, enclosed) = match unenclosed {
        Some(begin) => (begin, false),
        None => match enclosed_title_begin(tape) {
            Some(begin) => (begin, true),
            None => return Verdict::nothing(),
        },
    };
    if !enclosed && leads_with_episode(tape, reading, begin) {
        return Verdict::nothing();
    }
    let mut end = tape.run_end(begin, enclosed);
    if enclosed {
        return Verdict::nothing().take_run(
            ElementKind::AnimeTitle,
            begin..end,
            Delimiters::Folded,
        );
    }

    let mut last_bracket = end;
    let mut bracket_open = false;
    for at in begin..end {
        if tape.tokens[at].is_bracket() {
            last_bracket = at;
            bracket_open = !bracket_open;
        }
    }
    if bracket_open {
        end = last_bracket;
    }

    let mut last = tape.prev(end, token::is_not_delimiter);
    while let Some(at) = last
        && tape.tokens[at].is_bracket()
        && !tape.tokens[at].text.starts_with(')')
    {
        let Some(opener) = tape.prev(at, Token::is_bracket) else {
            break;
        };
        end = opener;
        last = tape.prev(end, token::is_not_delimiter);
    }

    Verdict::nothing().take_run(ElementKind::AnimeTitle, begin..end, Delimiters::Folded)
}

fn leads_with_episode(tape: &Tape, reading: &Reading, begin: usize) -> bool {
    let is_dash = |at: usize| string::is_dash(&tape.tokens[at].text);
    episode_token(tape, reading).is_some_and(|episode| episode < begin)
        && (is_dash(begin)
            || tape
                .prev(begin, token::is_not_delimiter)
                .is_some_and(is_dash))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::probe;

    fn title(input: &str) -> Vec<(ElementKind, String)> {
        probe(RuleName::Title, input)
    }

    #[test]
    fn the_free_run_outside_the_brackets_is_the_title() {
        assert_eq!(
            title("[Group] Show Name - 03 [720p].mkv"),
            [(ElementKind::AnimeTitle, "Show Name".to_owned())]
        );
    }

    #[test]
    fn a_parenthesized_tail_stays_in_the_title() {
        assert_eq!(
            title("[Group] Anime (TV) - 03 [720p].mkv"),
            [(ElementKind::AnimeTitle, "Anime (TV)".to_owned())]
        );
        assert_eq!(
            title("[Group] Anime [TV] - 03 [720p].mkv"),
            [(ElementKind::AnimeTitle, "Anime".to_owned())]
        );
    }

    #[test]
    fn the_second_bracket_group_is_the_title_when_nothing_stands_outside() {
        assert_eq!(
            title("[Group][Show Name][03][720p].mkv"),
            [(ElementKind::AnimeTitle, "Show Name".to_owned())]
        );
    }

    #[test]
    fn a_name_that_leads_with_its_episode_and_a_dash_has_no_title() {
        assert!(title("03 - The Hidden Village.mkv").is_empty());
        assert_eq!(
            title("Episode 14 Ore no Imouto.mkv"),
            [(ElementKind::AnimeTitle, "Ore no Imouto".to_owned())]
        );
    }
}
