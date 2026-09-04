use super::Parser;
use crate::element::ElementKind;
use crate::string;
use crate::token::{self, TokenCategory};

const ANIME_YEAR_MIN: u32 = 1900;
const ANIME_YEAR_MAX: u32 = 2050;
const EPISODE_NUMBER_MAX: u32 = ANIME_YEAR_MIN - 1;
const VOLUME_NUMBER_MAX: u32 = 20;

/// A number counted off against a prefix. Episodes and volumes differ only in
/// their bounds and in whether a second number retags the first.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::parser) enum Extent {
    Episode,
    Volume,
}

impl Extent {
    fn prefix(self) -> ElementKind {
        match self {
            Extent::Episode => ElementKind::EpisodePrefix,
            Extent::Volume => ElementKind::VolumePrefix,
        }
    }

    fn number(self) -> ElementKind {
        match self {
            Extent::Episode => ElementKind::EpisodeNumber,
            Extent::Volume => ElementKind::VolumeNumber,
        }
    }

    fn max_digits(self) -> usize {
        match self {
            Extent::Episode => 4,
            Extent::Volume => 2,
        }
    }

    fn max_value(self) -> u32 {
        match self {
            Extent::Episode => EPISODE_NUMBER_MAX,
            Extent::Volume => VOLUME_NUMBER_MAX,
        }
    }
}

mod patterns;
mod scanner;

/// A digit run that overflows `u32` compares as larger than every bound, so
/// the validation checks reject it.
pub(super) fn leading_value(text: &str) -> u32 {
    string::leading_number(text).unwrap_or(u32::MAX)
}

impl Parser<'_> {
    pub(in crate::parser) fn search_isolated_numbers(&mut self) {
        for index in 0..self.tokens.len() {
            if self.tokens[index].category != TokenCategory::Unknown
                || !string::is_numeric(&self.tokens[index].content)
                || !self.is_isolated(index)
            {
                continue;
            }
            let Some(number) = string::leading_number(&self.tokens[index].content) else {
                continue;
            };
            if (ANIME_YEAR_MIN..=ANIME_YEAR_MAX).contains(&number)
                && !self.elements.contains(ElementKind::AnimeYear)
            {
                let content = self.tokens[index].content.clone();
                self.elements.insert(ElementKind::AnimeYear, content);
                self.tokens[index].category = TokenCategory::Identifier;
                continue;
            }
            if matches!(number, 480 | 720 | 1080)
                && !self.elements.contains(ElementKind::VideoResolution)
            {
                let content = self.tokens[index].content.clone();
                self.elements.insert(ElementKind::VideoResolution, content);
                self.tokens[index].category = TokenCategory::Identifier;
            }
        }
    }

    pub(in crate::parser) fn search_episode_number(&mut self) {
        let candidates: Vec<usize> = (0..self.tokens.len())
            .filter(|&index| {
                self.tokens[index].category == TokenCategory::Unknown
                    && self.tokens[index]
                        .content
                        .chars()
                        .any(|c| c.is_ascii_digit())
            })
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
            .filter(|&index| string::is_numeric(&self.tokens[index].content))
            .collect();
        if numeric.is_empty() {
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

    pub(super) fn is_isolated(&self, index: usize) -> bool {
        let is_bracket = |index: Option<usize>| {
            index.is_some_and(|index| self.tokens[index].category == TokenCategory::Bracket)
        };
        is_bracket(token::find_prev(
            &self.tokens,
            index,
            token::is_not_delimiter,
        )) && is_bracket(token::find_next(
            &self.tokens,
            index,
            token::is_not_delimiter,
        ))
    }

    fn search_episode_patterns(&mut self, candidates: &[usize]) -> bool {
        for &index in candidates {
            let starts_with_digit = self.tokens[index]
                .content
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
            let word = self.tokens[index].content.clone();
            if self.match_patterns(Extent::Episode, &word, index) {
                return true;
            }
        }
        false
    }

    fn number_comes_after_prefix(&mut self, index: usize) -> Option<Extent> {
        let content = self.tokens[index].content.clone();
        let digit_pos = content.find(|c: char| c.is_ascii_digit())?;
        let prefix = content[..digit_pos].to_uppercase();
        let extent = [Extent::Episode, Extent::Volume]
            .into_iter()
            .find(|extent| self.table.find(extent.prefix(), &prefix).is_some())?;
        self.claim_number(extent, &content[digit_pos..], index);
        Some(extent)
    }

    fn number_comes_before_another_number(&mut self, index: usize) -> bool {
        let Some(separator) = token::find_next(&self.tokens, index, token::is_not_delimiter) else {
            return false;
        };
        let content = &self.tokens[separator].content;
        let includes_other = if content == "&" {
            true
        } else if content.eq_ignore_ascii_case("of") {
            false
        } else {
            return false;
        };
        let Some(other) = token::find_next(&self.tokens, separator, token::is_not_delimiter) else {
            return false;
        };
        if !string::is_numeric(&self.tokens[other].content) {
            return false;
        }
        let number = self.tokens[index].content.clone();
        self.set_number(Extent::Episode, &number, index, false);
        if includes_other {
            let number = self.tokens[other].content.clone();
            self.set_number(Extent::Episode, &number, other, false);
        }
        self.tokens[separator].category = TokenCategory::Identifier;
        self.tokens[other].category = TokenCategory::Identifier;
        true
    }
    fn search_separated_numbers(&mut self, numeric: &[usize]) -> bool {
        for &index in numeric {
            let Some(prev) = token::find_prev(&self.tokens, index, token::is_not_delimiter) else {
                continue;
            };
            if self.tokens[prev].category == TokenCategory::Unknown
                && string::is_dash(&self.tokens[prev].content)
            {
                let number = self.tokens[index].content.clone();
                if self.set_number(Extent::Episode, &number, index, true) {
                    self.tokens[prev].category = TokenCategory::Identifier;
                    return true;
                }
            }
        }
        false
    }

    fn search_isolated_episode_numbers(&mut self, numeric: &[usize]) -> bool {
        for &index in numeric {
            if !self.tokens[index].enclosed || !self.is_isolated(index) {
                continue;
            }
            let number = self.tokens[index].content.clone();
            if self.set_number(Extent::Episode, &number, index, true) {
                return true;
            }
        }
        false
    }

    fn search_last_number(&mut self, numeric: &[usize]) -> bool {
        for &index in numeric.iter().rev() {
            if index == 0 || self.tokens[index].enclosed {
                continue;
            }
            if self.tokens[..index]
                .iter()
                .all(|token| token.enclosed || token.category == TokenCategory::Delimiter)
            {
                continue;
            }
            if let Some(prev) = token::find_prev(&self.tokens, index, token::is_not_delimiter)
                && self.tokens[prev].category == TokenCategory::Unknown
                && (self.tokens[prev].content.eq_ignore_ascii_case("movie")
                    || self.tokens[prev].content.eq_ignore_ascii_case("part"))
            {
                continue;
            }
            let number = self.tokens[index].content.clone();
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
        self.tokens[index].category = TokenCategory::Identifier;
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
                return false;
            }
        }
        self.elements.insert(kind, number);
        true
    }

    /// Reads the number through the pattern matchers, falling back to taking
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
