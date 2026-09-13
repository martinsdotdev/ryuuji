//! The word-level matchers: given one token's text, decide whether it spells
//! a numbered extent and record it.

use super::scanner::{Scanner, eat_episode_separator, version};
use super::{Extent, leading_value};
use crate::element::ElementKind;
use crate::parser::Parser;
use crate::string;

impl Parser<'_> {
    pub(in crate::parser) fn match_patterns(
        &mut self,
        extent: Extent,
        word: &str,
        index: usize,
    ) -> bool {
        if string::is_numeric(word) {
            return false;
        }
        let word = word.trim_matches([' ', '-']);
        if word.is_empty() {
            return false;
        }
        let chars: Vec<char> = word.chars().collect();
        if self.match_single(extent, &chars, index) || self.match_multi(extent, &chars, index) {
            return true;
        }
        if extent == Extent::Volume {
            return false;
        }
        self.match_season_and_episode(&chars, index)
            || self.match_type_and_episode(word, index)
            || self.match_fractional_episode(&chars, word, index)
            || self.match_partial_episode(&chars, word, index)
            || self.match_number_sign(&chars, index)
            || self.match_japanese_counter(&chars, index)
    }

    fn match_single(&mut self, extent: Extent, chars: &[char], index: usize) -> bool {
        let mut scanner = Scanner::new(chars);
        let Some(number) = scanner.digits(extent.max_digits()) else {
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
        self.set_number(extent, &number, index, false);
        self.record(ElementKind::ReleaseVersion, release_version, index);
        true
    }

    fn match_multi(&mut self, extent: Extent, chars: &[char], index: usize) -> bool {
        let mut scanner = Scanner::new(chars);
        let Some(lower) = scanner.digits(extent.max_digits()) else {
            return false;
        };
        // Only an episode range carries a version on its lower bound.
        let lower_version = match extent {
            Extent::Episode => version(&mut scanner),
            Extent::Volume => None,
        };
        if !scanner.eat_any(&['-', '~', '&', '+']) {
            return false;
        }
        let Some(upper) = scanner.digits(extent.max_digits()) else {
            return false;
        };
        let upper_version = version(&mut scanner);
        if !scanner.done() {
            return false;
        }
        if leading_value(&lower) >= leading_value(&upper) {
            return false;
        }
        if !self.set_number(extent, &lower, index, true) {
            return false;
        }
        self.set_number(extent, &upper, index, false);
        for value in [lower_version, upper_version].into_iter().flatten() {
            self.record(ElementKind::ReleaseVersion, value, index);
        }
        true
    }

    fn match_season_and_episode(&mut self, chars: &[char], index: usize) -> bool {
        let mut scanner = Scanner::new(chars);
        scanner.eat_any(&['S', 's']);
        let Some(first_season) = scanner.digits(2) else {
            return false;
        };
        let second_season = scanner.attempt(|scanner| {
            scanner.eat('-').then(|| {
                scanner.eat_any(&['S', 's']);
                scanner.digits(2)
            })?
        });
        if !eat_episode_separator(&mut scanner) {
            return false;
        }
        let Some(first_episode) = scanner.digits(4) else {
            return false;
        };
        let second_episode = scanner.attempt(|scanner| {
            scanner.eat('-').then(|| {
                scanner.eat_any(&['E', 'e']);
                scanner.digits(4)
            })?
        });
        let release_version = version(&mut scanner);
        if !scanner.done() {
            return false;
        }
        if leading_value(&first_season) == 0 {
            return false;
        }
        self.record(ElementKind::AnimeSeason, first_season, index);
        if let Some(season) = second_season {
            self.record(ElementKind::AnimeSeason, season, index);
        }
        self.set_number(Extent::Episode, &first_episode, index, false);
        if let Some(episode) = second_episode {
            self.set_number(Extent::Episode, &episode, index, false);
        }
        if let Some(value) = release_version {
            self.record(ElementKind::ReleaseVersion, value, index);
        }
        true
    }

    /// `OVA1`: the token is cut in two so the type can stay a title word
    /// while the number is taken.
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
        let split = self.tape.tokens[index].text.find(word).unwrap_or(0) + digit_pos;
        self.tape.split(index, split);
        self.record(ElementKind::AnimeType, prefix, index);
        if keyword.identifiable {
            self.retire(index, ElementKind::AnimeType);
        } else {
            self.hold(index, ElementKind::AnimeType);
        }
        self.claim_number(Extent::Episode, &number, index + 1);
        true
    }

    /// `07.5` is an episode; `1.11` and `8.0` are parts of titles and `5.1`
    /// is an audio term, so any other fraction needs a leading zero
    /// (`04.1`) before it counts, since no title or term writes one.
    fn match_fractional_episode(&mut self, chars: &[char], word: &str, index: usize) -> bool {
        let mut scanner = Scanner::new(chars);
        let Some(whole) = scanner.digits(usize::MAX) else {
            return false;
        };
        if !scanner.eat('.') {
            return false;
        }
        let leading_zero = whole.len() > 1 && whole.starts_with('0');
        let fraction_ok = if leading_zero {
            scanner.digits(1).is_some()
        } else {
            scanner.eat('5')
        };
        if !fraction_ok || !scanner.done() {
            return false;
        }
        self.set_number(Extent::Episode, word, index, true)
    }

    fn match_partial_episode(&mut self, chars: &[char], word: &str, index: usize) -> bool {
        let digits = chars.iter().take_while(|c| c.is_ascii_digit()).count();
        let suffix = &chars[digits..];
        if suffix.len() == 1 && matches!(suffix[0], 'A'..='C' | 'a'..='c') {
            return self.set_number(Extent::Episode, word, index, true);
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
        let second = scanner.attempt(|scanner| {
            scanner
                .eat_any(&['-', '~', '&', '+'])
                .then(|| scanner.digits(4))?
        });
        let release_version = version(&mut scanner);
        if !scanner.done() {
            return false;
        }
        if !self.set_number(Extent::Episode, &first, index, true) {
            return false;
        }
        if let Some(episode) = second {
            self.set_number(Extent::Episode, &episode, index, true);
        }
        if let Some(value) = release_version {
            self.record(ElementKind::ReleaseVersion, value, index);
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
        self.set_number(Extent::Episode, &episode, index, false);
        true
    }
}
