use super::Parser;
use crate::element::ElementKind;
use crate::numbering::{EPISODE_NUMBER_MAX, Extent, leading_value};
use crate::string;
use crate::token;

impl Parser<'_> {
    pub(in crate::parser) fn search_episode_number(&mut self) {
        if self.elements.contains(ElementKind::EpisodeNumber) {
            return;
        }
        let numeric: Vec<usize> = self
            .tape
            .free()
            .filter(|(_, token)| token.numeric)
            .map(|(index, _)| index)
            .collect();
        if numeric.is_empty() {
            return;
        }

        if self.search_equivalent_numbers(&numeric) {
            return;
        }
        if self.search_separated_numbers(&numeric) {
            return;
        }
        if self.search_isolated_episode_numbers(&numeric) {
            return;
        }
        self.search_last_number(&numeric);
    }

    /// `02 (100)`: a plain number followed by an isolated one in brackets is
    /// one episode under two numbering schemes. The smaller is the episode
    /// and the larger the alternative, whichever comes first.
    fn search_equivalent_numbers(&mut self, numeric: &[usize]) -> bool {
        let within_bound =
            |index: usize| leading_value(&self.tape.tokens[index].text) <= EPISODE_NUMBER_MAX;
        for &index in numeric {
            if self.tape.isolated(index) || !within_bound(index) {
                continue;
            }
            let Some(bracket) = self.tape.next(index, token::is_not_delimiter) else {
                continue;
            };
            if !self.tape.tokens[bracket].is_bracket() {
                continue;
            }
            let Some(other) = self.tape.next(bracket, |token| {
                token.enclosed && token::is_not_delimiter(token)
            }) else {
                continue;
            };
            if !self.tape.tokens[other].is_free()
                || !self.tape.tokens[other].numeric
                || !self.tape.isolated(other)
                || !within_bound(other)
            {
                continue;
            }
            let (episode, alt) = if leading_value(&self.tape.tokens[other].text)
                < leading_value(&self.tape.tokens[index].text)
            {
                (other, index)
            } else {
                (index, other)
            };
            let number = self.tape.tokens[episode].text.clone();
            self.set_number(Extent::Episode, &number, episode, false);
            let number = self.tape.tokens[alt].text.clone();
            self.record(ElementKind::EpisodeNumberAlt, number, alt);
            self.retire(alt, ElementKind::EpisodeNumberAlt);
            return true;
        }
        false
    }

    /// A number set off by a dash: `Title - 08`, or `08 - Episode Title` when
    /// the number opens the name and nothing but the dash follows it.
    fn search_separated_numbers(&mut self, numeric: &[usize]) -> bool {
        for &index in numeric {
            let dash = |neighbour: Option<usize>| {
                neighbour.filter(|&neighbour| {
                    self.tape.tokens[neighbour].is_free()
                        && string::is_dash(&self.tape.tokens[neighbour].text)
                })
            };
            let prev = self.tape.prev(index, token::is_not_delimiter);
            let separator = match dash(prev) {
                Some(prev) => prev,
                None if prev.is_none() => {
                    let Some(next) = dash(self.tape.next(index, token::is_not_delimiter)) else {
                        continue;
                    };
                    next
                }
                None => continue,
            };
            let number = self.tape.tokens[index].text.clone();
            if self.set_number(Extent::Episode, &number, index, true) {
                self.retire(separator, ElementKind::EpisodeNumber);
                return true;
            }
        }
        false
    }

    fn search_isolated_episode_numbers(&mut self, numeric: &[usize]) -> bool {
        for &index in numeric {
            if !self.tape.tokens[index].enclosed || !self.tape.isolated(index) {
                continue;
            }
            let number = self.tape.tokens[index].text.clone();
            if self.set_number(Extent::Episode, &number, index, true) {
                return true;
            }
        }
        false
    }

    fn search_last_number(&mut self, numeric: &[usize]) -> bool {
        for &index in numeric.iter().rev() {
            if index == 0 || self.tape.tokens[index].enclosed {
                continue;
            }
            // The episode comes after the title, so the first token outside
            // brackets is not it, unless it is the only one and a title
            // waits inside a bracket group (`[Group][Title] 02 [720p]`).
            let first_outside = self.tape.tokens[..index]
                .iter()
                .all(|token| token.enclosed || token.is_delimiter());
            let only_outside = !self.tape.tokens[index + 1..]
                .iter()
                .any(|token| !token.enclosed && token.is_free());
            if first_outside && !(only_outside && self.enclosed_title_begin().is_some()) {
                continue;
            }
            if let Some(prev) = self.tape.prev(index, token::is_not_delimiter)
                && self.tape.tokens[prev].is_free()
                && (self.tape.tokens[prev].text.eq_ignore_ascii_case("movie")
                    || self.tape.tokens[prev].text.eq_ignore_ascii_case("part"))
            {
                continue;
            }
            let number = self.tape.tokens[index].text.clone();
            if self.set_number(Extent::Episode, &number, index, true) {
                return true;
            }
        }
        false
    }

    pub(super) fn set_number(
        &mut self,
        extent: Extent,
        number: &str,
        index: usize,
        validate: bool,
    ) -> bool {
        if validate && leading_value(number) > extent.max_value() {
            return false;
        }
        if extent == Extent::Episode && self.episode_token.is_none() {
            self.episode_token = Some(index);
        }
        self.retire(index, extent.number());
        self.record(extent.number(), number, index);
        true
    }
}
