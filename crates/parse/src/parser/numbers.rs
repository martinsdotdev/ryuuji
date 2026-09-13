use super::Parser;
use crate::element::ElementKind;
use crate::numbering::{
    self, ANIME_YEAR_MAX, ANIME_YEAR_MIN, EPISODE_NUMBER_MAX, Extent, leading_value,
};
use crate::string;
use crate::token;

impl Parser<'_> {
    pub(in crate::parser) fn search_isolated_numbers(&mut self) {
        for index in 0..self.tape.len() {
            if !self.tape.tokens[index].is_free()
                || !self.tape.tokens[index].numeric
                || !self.tape.isolated(index)
            {
                continue;
            }
            let Some(number) = string::leading_number(&self.tape.tokens[index].text) else {
                continue;
            };
            if (ANIME_YEAR_MIN..=ANIME_YEAR_MAX).contains(&number)
                && !self.elements.contains(ElementKind::AnimeYear)
            {
                let text = self.tape.tokens[index].text.clone();
                self.record(ElementKind::AnimeYear, text, index);
                self.retire(index, ElementKind::AnimeYear);
                continue;
            }
            if matches!(number, 480 | 720 | 1080)
                && !self.elements.contains(ElementKind::VideoResolution)
            {
                let text = self.tape.tokens[index].text.clone();
                self.record(ElementKind::VideoResolution, text, index);
                self.retire(index, ElementKind::VideoResolution);
            }
        }
    }

    pub(in crate::parser) fn search_episode_number(&mut self) {
        let candidates: Vec<usize> = self
            .tape
            .free()
            .filter(|(_, token)| token.text.chars().any(|c| c.is_ascii_digit()))
            .map(|(index, _)| index)
            .collect();
        if candidates.is_empty() {
            return;
        }

        self.found_episode_keyword = self.elements.contains(ElementKind::EpisodeNumber);

        if self.search_episode_patterns(&candidates) {
            return;
        }
        if self.elements.contains(ElementKind::EpisodeNumber) {
            return;
        }

        let numeric: Vec<usize> = candidates
            .into_iter()
            .filter(|&index| self.tape.tokens[index].numeric)
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

    fn search_episode_patterns(&mut self, candidates: &[usize]) -> bool {
        // A pattern may split its token in two, shifting every index after
        // it, so the candidates are walked by value and re-read.
        let mut offset = 0;
        for &candidate in candidates {
            let index = candidate + offset;
            let starts_with_digit = self.tape.tokens[index]
                .text
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit());
            if !starts_with_digit {
                match self.number_comes_after_prefix(index) {
                    Some(Extent::Episode) => return true,
                    Some(Extent::Volume) => continue,
                    None => {}
                }
            } else if self.number_comes_before_another_number(index) {
                return true;
            }
            let word = self.tape.tokens[index].text.clone();
            let before = self.tape.len();
            if self.match_patterns(Extent::Episode, &word, index) {
                return true;
            }
            offset += self.tape.len() - before;
        }
        false
    }

    fn number_comes_after_prefix(&mut self, index: usize) -> Option<Extent> {
        let text = self.tape.tokens[index].text.clone();
        let digit_pos = text.find(|c: char| c.is_ascii_digit())?;
        let prefix = text[..digit_pos].to_uppercase();
        let extent = [Extent::Episode, Extent::Volume]
            .into_iter()
            .find(|extent| self.table.find(extent.prefix(), &prefix).is_some())?;
        self.claim_number(extent, &text[digit_pos..], index);
        Some(extent)
    }

    fn number_comes_before_another_number(&mut self, index: usize) -> bool {
        let Some(separator) = self.tape.next(index, token::is_not_delimiter) else {
            return false;
        };
        let text = &self.tape.tokens[separator].text;
        let includes_other = if text == "&" {
            true
        } else if text.eq_ignore_ascii_case("of") {
            false
        } else {
            return false;
        };
        let Some(other) = self.tape.next(separator, token::is_not_delimiter) else {
            return false;
        };
        if !self.tape.tokens[other].numeric {
            return false;
        }
        let number = self.tape.tokens[index].text.clone();
        self.set_number(Extent::Episode, &number, index, false);
        if includes_other {
            let number = self.tape.tokens[other].text.clone();
            self.set_number(Extent::Episode, &number, other, false);
        }
        self.retire(separator, ElementKind::EpisodeNumber);
        self.retire(other, ElementKind::EpisodeNumber);
        true
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
        let mut kind = extent.number();
        if extent == Extent::Episode
            && self.found_episode_keyword
            && let Some(existing) = self.elements.get(ElementKind::EpisodeNumber)
        {
            let new = leading_value(number);
            let old = leading_value(existing);
            if new > old {
                kind = ElementKind::EpisodeNumberAlt;
            } else if new < old {
                self.elements
                    .retag_first(ElementKind::EpisodeNumber, ElementKind::EpisodeNumberAlt);
            } else {
                self.retire(index, kind);
                return false;
            }
        }
        self.retire(index, kind);
        self.record(kind, number, index);
        true
    }

    /// Reads the word through the number shapes and records what it spells.
    /// A type glued to its number (`OVA1`) cuts the token in two so the type
    /// can stay a title word.
    pub(in crate::parser) fn match_patterns(
        &mut self,
        extent: Extent,
        word: &str,
        index: usize,
    ) -> bool {
        let Some(numbering) = numbering::read(word, extent) else {
            return false;
        };
        let mut index = index;
        for piece in numbering.pieces {
            match piece.kind {
                ElementKind::AnimeType => {
                    let split =
                        self.tape.tokens[index].text.find(word).unwrap_or(0) + piece.part.at.end;
                    self.tape.split(index, split);
                    self.record(ElementKind::AnimeType, piece.part.text, index);
                    if piece.held {
                        self.hold(index, ElementKind::AnimeType);
                    } else {
                        self.retire(index, ElementKind::AnimeType);
                    }
                    index += 1;
                }
                ElementKind::EpisodeNumber | ElementKind::VolumeNumber => {
                    self.set_number(extent, &piece.part.text, index, false);
                }
                kind => self.record(kind, piece.part.text, index),
            }
        }
        true
    }

    /// Reads the number through the number shapes, falling back to taking
    /// it whole.
    pub(in crate::parser) fn claim_number(
        &mut self,
        extent: Extent,
        word: &str,
        index: usize,
    ) -> bool {
        self.match_patterns(extent, word, index) || self.set_number(extent, word, index, false)
    }
}
