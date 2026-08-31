use super::Parser;
use crate::element::ElementKind;
use crate::token::{self, TokenCategory};

impl Parser<'_> {
    pub(super) fn search_release_group(&mut self) {
        if self.elements.contains(ElementKind::ReleaseGroup) {
            return;
        }
        let len = self.tokens.len();
        let mut search_from = 0;
        loop {
            let Some(begin) = (search_from..len).find(|&index| {
                self.tokens[index].enclosed && self.tokens[index].category == TokenCategory::Unknown
            }) else {
                return;
            };
            let end = (begin..len)
                .find(|&index| {
                    matches!(
                        self.tokens[index].category,
                        TokenCategory::Bracket | TokenCategory::Identifier
                    )
                })
                .unwrap_or(len);
            search_from = end;
            if end == len || self.tokens[end].category != TokenCategory::Bracket {
                continue;
            }
            if let Some(prev) = token::find_prev(&self.tokens, begin, token::is_not_delimiter)
                && self.tokens[prev].category != TokenCategory::Bracket
            {
                continue;
            }
            self.build_and_insert(ElementKind::ReleaseGroup, begin, end, true);
            return;
        }
    }
}
