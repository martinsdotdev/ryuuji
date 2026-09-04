use super::Parser;
use crate::element::ElementKind;
use crate::token::{self, TokenCategory};

impl Parser<'_> {
    pub(super) fn search_release_group(&mut self) {
        if self.elements.contains(ElementKind::ReleaseGroup) {
            return;
        }
        let Some((begin, end)) = self.unknown_spans(true).find(|&(begin, end)| {
            end < self.tokens.len()
                && self.tokens[end].category == TokenCategory::Bracket
                && token::find_prev(&self.tokens, begin, token::is_not_delimiter)
                    .is_none_or(|prev| self.tokens[prev].category == TokenCategory::Bracket)
        }) else {
            return;
        };
        self.build_and_insert(ElementKind::ReleaseGroup, begin, end, true);
    }
}
