use super::Parser;
use crate::element::ElementKind;
use crate::numbering::{Extent, leading_value};
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

        self.search_last_number(&numeric);
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
