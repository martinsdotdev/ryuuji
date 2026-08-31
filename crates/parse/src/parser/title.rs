use super::Parser;
use crate::element::ElementKind;
use crate::token::{self, TokenCategory};

impl Parser<'_> {
    pub(super) fn search_anime_title(&mut self) {
        // An entirely-enclosed title (no unknown token outside brackets) is a
        // deferred case; such filenames end up with no anime title.
        let Some(begin) = self
            .tokens
            .iter()
            .position(|token| !token.enclosed && token.category == TokenCategory::Unknown)
        else {
            return;
        };
        let len = self.tokens.len();
        let mut end = (begin..len)
            .find(|&index| self.tokens[index].category == TokenCategory::Identifier)
            .unwrap_or(len);

        let mut last_bracket = end;
        let mut bracket_open = false;
        for index in begin..end {
            if self.tokens[index].category == TokenCategory::Bracket {
                last_bracket = index;
                bracket_open = !bracket_open;
            }
        }
        if bracket_open {
            end = last_bracket;
        }

        // Trailing enclosed groups are cut off, except parenthesized ones,
        // so a title like "Anime (TV)" stays intact.
        let mut last = token::find_prev(&self.tokens, end, token::is_not_delimiter);
        while let Some(index) = last
            && self.tokens[index].category == TokenCategory::Bracket
            && !self.tokens[index].content.starts_with(')')
        {
            let Some(opener) = token::find_prev(&self.tokens, index, |token| {
                token.category == TokenCategory::Bracket
            }) else {
                break;
            };
            end = opener;
            last = token::find_prev(&self.tokens, end, token::is_not_delimiter);
        }

        self.build_and_insert(ElementKind::AnimeTitle, begin, end, false);
    }
}
