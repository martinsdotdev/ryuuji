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
                && self.tokens[end].is_bracket()
                && token::find_prev(&self.tokens, begin, token::is_not_delimiter)
                    .is_none_or(|prev| self.tokens[prev].is_bracket())
        }) else {
            self.search_trailing_release_group();
            return;
        };
        self.build_and_insert(ElementKind::ReleaseGroup, begin, end, true);
    }

    /// Scene-style naming hangs the group off the end after a dash
    /// (`...(BDrip 1920x1080 x264)-ank.mkv`); the dash glues onto the word,
    /// so the last token outside the brackets reads `-ank`.
    fn search_trailing_release_group(&mut self) {
        let Some(last) = token::find_prev(&self.tokens, self.tokens.len(), token::is_not_delimiter)
        else {
            return;
        };
        let token = &self.tokens[last];
        if token.enclosed || token.category != TokenCategory::Unknown {
            return;
        }
        let Some(group) = token.content.strip_prefix('-') else {
            return;
        };
        if group.is_empty() || group.contains('-') {
            return;
        }
        let after_bracket = token::find_prev(&self.tokens, last, token::is_not_delimiter)
            .is_some_and(|prev| self.tokens[prev].is_bracket());
        if !after_bracket {
            return;
        }
        let group = group.to_owned();
        self.tokens[last].category = TokenCategory::Identifier;
        self.elements.insert(ElementKind::ReleaseGroup, group);
    }
}
