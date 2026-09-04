use super::Parser;
use crate::element::ElementKind;
use crate::string;
use crate::token::{self, Token, TokenCategory};

const ANIME_YEAR_MIN: u32 = 1900;
const ANIME_YEAR_MAX: u32 = 2050;
const EPISODE_NUMBER_MAX: u32 = ANIME_YEAR_MIN - 1;
const VOLUME_NUMBER_MAX: u32 = 20;

struct Scanner<'a> {
    chars: &'a [char],
    pos: usize,
}

impl<'a> Scanner<'a> {
    fn new(chars: &'a [char]) -> Scanner<'a> {
        Scanner { chars, pos: 0 }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn eat(&mut self, expected: char) -> bool {
        if self.peek() == Some(expected) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn eat_any(&mut self, options: &[char]) -> bool {
        if self.peek().is_some_and(|c| options.contains(&c)) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn digits(&mut self, max: usize) -> Option<String> {
        let start = self.pos;
        while self.pos - start < max && self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }
        (self.pos > start).then(|| self.chars[start..self.pos].iter().collect())
    }

    fn done(&self) -> bool {
        self.pos == self.chars.len()
    }
}

/// A digit run that overflows `u32` compares as larger than every bound, so
/// the validation checks reject it.
fn leading_value(text: &str) -> u32 {
    string::leading_number(text).unwrap_or(u32::MAX)
}

fn version(scanner: &mut Scanner) -> Option<String> {
    let saved = scanner.pos;
    if scanner.eat_any(&['v', 'V'])
        && let Some(digit) = scanner.digits(1)
    {
        return Some(digit);
    }
    scanner.pos = saved;
    None
}

fn eat_episode_separator(scanner: &mut Scanner) -> bool {
    let saved = scanner.pos;
    if scanner.eat_any(&[' ', '.', '_', '-', 'x', 'X']) && scanner.eat_any(&['E', 'e']) {
        return true;
    }
    scanner.pos = saved;
    scanner.eat_any(&['E', 'e', 'x', 'X'])
}

impl Parser<'_> {
    pub(super) fn search_isolated_numbers(&mut self) {
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

    pub(super) fn search_episode_number(&mut self) {
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

    fn is_isolated(&self, index: usize) -> bool {
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
                    Some(ElementKind::EpisodeNumber) => return true,
                    Some(_) => continue,
                    None => {}
                }
            } else if self.number_comes_before_another_number(index) {
                return true;
            }
            let word = self.tokens[index].content.clone();
            if self.match_episode_patterns(&word, index) {
                return true;
            }
        }
        false
    }

    fn number_comes_after_prefix(&mut self, index: usize) -> Option<ElementKind> {
        let content = self.tokens[index].content.clone();
        let digit_pos = content.find(|c: char| c.is_ascii_digit())?;
        let prefix = content[..digit_pos].to_uppercase();
        let number = &content[digit_pos..];
        if self
            .table
            .find(ElementKind::EpisodePrefix, &prefix)
            .is_some()
        {
            if !self.match_episode_patterns(number, index) {
                self.set_episode(number, index, false);
            }
            return Some(ElementKind::EpisodeNumber);
        }
        if self
            .table
            .find(ElementKind::VolumePrefix, &prefix)
            .is_some()
        {
            if !self.match_volume_patterns(number, index) {
                self.set_volume(number, index, false);
            }
            return Some(ElementKind::VolumeNumber);
        }
        None
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
        self.set_episode(&number, index, false);
        if includes_other {
            let number = self.tokens[other].content.clone();
            self.set_episode(&number, other, false);
        }
        self.tokens[separator].category = TokenCategory::Identifier;
        self.tokens[other].category = TokenCategory::Identifier;
        true
    }

    pub(super) fn match_episode_patterns(&mut self, word: &str, index: usize) -> bool {
        if string::is_numeric(word) {
            return false;
        }
        let word = word.trim_matches([' ', '-']);
        if word.is_empty() {
            return false;
        }
        let chars: Vec<char> = word.chars().collect();
        let numeric_front = chars[0].is_ascii_digit();
        let numeric_back = chars[chars.len() - 1].is_ascii_digit();

        if numeric_front && numeric_back && self.match_single_episode(&chars, index) {
            return true;
        }
        if numeric_front && numeric_back && self.match_multi_episode(&chars, index) {
            return true;
        }
        if numeric_back && self.match_season_and_episode(&chars, index) {
            return true;
        }
        if !numeric_front && self.match_type_and_episode(word, index) {
            return true;
        }
        if numeric_front && numeric_back && self.match_fractional_episode(&chars, word, index) {
            return true;
        }
        if numeric_front && !numeric_back && self.match_partial_episode(&chars, word, index) {
            return true;
        }
        if numeric_back && self.match_number_sign(&chars, index) {
            return true;
        }
        if numeric_front && self.match_japanese_counter(&chars, index) {
            return true;
        }
        false
    }

    fn match_single_episode(&mut self, chars: &[char], index: usize) -> bool {
        let mut scanner = Scanner::new(chars);
        let Some(episode) = scanner.digits(4) else {
            return false;
        };
        if !scanner.eat_any(&['v', 'V']) {
            return false;
        }
        let Some(release_version) = scanner.digits(1) else {
            return false;
        };
        if !scanner.done() {
            return false;
        }
        self.set_episode(&episode, index, false);
        self.elements
            .insert(ElementKind::ReleaseVersion, release_version);
        true
    }

    fn match_multi_episode(&mut self, chars: &[char], index: usize) -> bool {
        let mut scanner = Scanner::new(chars);
        let Some(lower) = scanner.digits(4) else {
            return false;
        };
        let lower_version = version(&mut scanner);
        if !scanner.eat_any(&['-', '~', '&', '+']) {
            return false;
        }
        let Some(upper) = scanner.digits(4) else {
            return false;
        };
        let upper_version = version(&mut scanner);
        if !scanner.done() {
            return false;
        }
        if leading_value(&lower) >= leading_value(&upper) {
            return false;
        }
        if !self.set_episode(&lower, index, true) {
            return false;
        }
        self.set_episode(&upper, index, false);
        if let Some(value) = lower_version {
            self.elements.insert(ElementKind::ReleaseVersion, value);
        }
        if let Some(value) = upper_version {
            self.elements.insert(ElementKind::ReleaseVersion, value);
        }
        true
    }

    fn match_season_and_episode(&mut self, chars: &[char], index: usize) -> bool {
        let mut scanner = Scanner::new(chars);
        scanner.eat_any(&['S', 's']);
        let Some(first_season) = scanner.digits(2) else {
            return false;
        };
        let mut second_season = None;
        let saved = scanner.pos;
        if scanner.eat('-') {
            scanner.eat_any(&['S', 's']);
            match scanner.digits(2) {
                Some(digits) => second_season = Some(digits),
                None => scanner.pos = saved,
            }
        }
        if !eat_episode_separator(&mut scanner) {
            return false;
        }
        let Some(first_episode) = scanner.digits(4) else {
            return false;
        };
        let mut second_episode = None;
        let saved = scanner.pos;
        if scanner.eat('-') {
            scanner.eat_any(&['E', 'e']);
            match scanner.digits(4) {
                Some(digits) => second_episode = Some(digits),
                None => scanner.pos = saved,
            }
        }
        version(&mut scanner);
        if !scanner.done() {
            return false;
        }
        if leading_value(&first_season) == 0 {
            return false;
        }
        self.elements.insert(ElementKind::AnimeSeason, first_season);
        if let Some(season) = second_season {
            self.elements.insert(ElementKind::AnimeSeason, season);
        }
        self.set_episode(&first_episode, index, false);
        if let Some(episode) = second_episode {
            self.set_episode(&episode, index, false);
        }
        true
    }

    fn match_type_and_episode(&mut self, word: &str, index: usize) -> bool {
        let Some(digit_pos) = word.find(|c: char| c.is_ascii_digit()) else {
            return false;
        };
        let prefix = &word[..digit_pos];
        let Some(keyword) = self
            .table
            .find(ElementKind::AnimeType, &prefix.to_uppercase())
        else {
            return false;
        };
        let prefix = prefix.to_owned();
        let number = word[digit_pos..].to_owned();
        self.elements.insert(ElementKind::AnimeType, prefix.clone());
        if self.match_episode_patterns(&number, index) || self.set_episode(&number, index, false) {
            let enclosed = self.tokens[index].enclosed;
            self.tokens[index].content = number;
            self.tokens.insert(
                index,
                Token {
                    category: if keyword.identifiable {
                        TokenCategory::Identifier
                    } else {
                        TokenCategory::Unknown
                    },
                    content: prefix,
                    enclosed,
                    kind: None,
                },
            );
            return true;
        }
        false
    }

    fn match_fractional_episode(&mut self, chars: &[char], word: &str, index: usize) -> bool {
        let mut scanner = Scanner::new(chars);
        if scanner.digits(usize::MAX).is_none()
            || !scanner.eat('.')
            || !scanner.eat('5')
            || !scanner.done()
        {
            return false;
        }
        self.set_episode(word, index, true)
    }

    fn match_partial_episode(&mut self, chars: &[char], word: &str, index: usize) -> bool {
        let digits = chars.iter().take_while(|c| c.is_ascii_digit()).count();
        let suffix = &chars[digits..];
        if suffix.len() == 1 && matches!(suffix[0], 'A'..='C' | 'a'..='c') {
            return self.set_episode(word, index, true);
        }
        false
    }

    fn match_number_sign(&mut self, chars: &[char], index: usize) -> bool {
        if chars.first() != Some(&'#') {
            return false;
        }
        let mut scanner = Scanner::new(&chars[1..]);
        let Some(first) = scanner.digits(4) else {
            return false;
        };
        let mut second = None;
        let saved = scanner.pos;
        if scanner.eat_any(&['-', '~', '&', '+']) {
            match scanner.digits(4) {
                Some(digits) => second = Some(digits),
                None => scanner.pos = saved,
            }
        }
        let release_version = version(&mut scanner);
        if !scanner.done() {
            return false;
        }
        if !self.set_episode(&first, index, true) {
            return false;
        }
        if let Some(episode) = second {
            self.set_episode(&episode, index, true);
        }
        if let Some(value) = release_version {
            self.elements.insert(ElementKind::ReleaseVersion, value);
        }
        true
    }

    fn match_japanese_counter(&mut self, chars: &[char], index: usize) -> bool {
        if chars.last() != Some(&'話') {
            return false;
        }
        let mut scanner = Scanner::new(chars);
        let Some(episode) = scanner.digits(4) else {
            return false;
        };
        if !scanner.eat('話') || !scanner.done() {
            return false;
        }
        self.set_episode(&episode, index, false);
        true
    }

    pub(super) fn match_volume_patterns(&mut self, word: &str, index: usize) -> bool {
        if string::is_numeric(word) {
            return false;
        }
        let word = word.trim_matches([' ', '-']);
        if word.is_empty() {
            return false;
        }
        let chars: Vec<char> = word.chars().collect();
        let numeric_front = chars[0].is_ascii_digit();
        let numeric_back = chars[chars.len() - 1].is_ascii_digit();

        if numeric_front && numeric_back && self.match_single_volume(&chars, index) {
            return true;
        }
        if numeric_front && numeric_back && self.match_multi_volume(&chars, index) {
            return true;
        }
        false
    }

    fn match_single_volume(&mut self, chars: &[char], index: usize) -> bool {
        let mut scanner = Scanner::new(chars);
        let Some(volume) = scanner.digits(2) else {
            return false;
        };
        if !scanner.eat_any(&['v', 'V']) {
            return false;
        }
        let Some(release_version) = scanner.digits(1) else {
            return false;
        };
        if !scanner.done() {
            return false;
        }
        self.set_volume(&volume, index, false);
        self.elements
            .insert(ElementKind::ReleaseVersion, release_version);
        true
    }

    fn match_multi_volume(&mut self, chars: &[char], index: usize) -> bool {
        let mut scanner = Scanner::new(chars);
        let Some(lower) = scanner.digits(2) else {
            return false;
        };
        if !scanner.eat_any(&['-', '~', '&', '+']) {
            return false;
        }
        let Some(upper) = scanner.digits(2) else {
            return false;
        };
        let release_version = version(&mut scanner);
        if !scanner.done() {
            return false;
        }
        if leading_value(&lower) >= leading_value(&upper) {
            return false;
        }
        if !self.set_volume(&lower, index, true) {
            return false;
        }
        self.set_volume(&upper, index, false);
        if let Some(value) = release_version {
            self.elements.insert(ElementKind::ReleaseVersion, value);
        }
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
                if self.set_episode(&number, index, true) {
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
            if self.set_episode(&number, index, true) {
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
            if self.set_episode(&number, index, true) {
                return true;
            }
        }
        false
    }

    pub(super) fn set_episode(&mut self, number: &str, index: usize, validate: bool) -> bool {
        if validate && leading_value(number) > EPISODE_NUMBER_MAX {
            return false;
        }
        self.tokens[index].category = TokenCategory::Identifier;
        let mut kind = ElementKind::EpisodeNumber;
        if self.found_episode_keyword
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

    pub(super) fn set_volume(&mut self, number: &str, index: usize, validate: bool) -> bool {
        if validate && leading_value(number) > VOLUME_NUMBER_MAX {
            return false;
        }
        self.elements.insert(ElementKind::VolumeNumber, number);
        self.tokens[index].category = TokenCategory::Identifier;
        true
    }
}
