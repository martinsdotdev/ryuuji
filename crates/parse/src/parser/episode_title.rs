use super::Parser;
use crate::element::ElementKind;
use crate::string;
use crate::token::TokenCategory;

impl Parser<'_> {
    pub(super) fn search_episode_title(&mut self) {
        if !self.elements.contains(ElementKind::EpisodeNumber) {
            return;
        }
        let len = self.tokens.len();
        let mut search_from = 0;
        loop {
            let Some(begin) = (search_from..len).find(|&index| {
                !self.tokens[index].enclosed
                    && self.tokens[index].category == TokenCategory::Unknown
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
            if end - begin <= 2 && string::is_dash(&self.tokens[begin].content) {
                continue;
            }
            self.build_and_insert(ElementKind::EpisodeTitle, begin, end, false);
            return;
        }
    }
}
