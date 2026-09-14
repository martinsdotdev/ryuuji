use super::Parser;
use crate::element::ElementKind;
use crate::rules::enclosed_title_begin;
use crate::string;
use crate::token::{self, Token};

impl Parser<'_> {
    pub(super) fn search_anime_title(&mut self) {
        let unenclosed = self
            .tape
            .iter()
            .find(|(_, token)| !token.enclosed && token.is_free())
            .map(|(index, _)| index);
        let (begin, enclosed) = match unenclosed {
            Some(begin) => (begin, false),
            None => match enclosed_title_begin(self.tape) {
                Some(begin) => (begin, true),
                None => return,
            },
        };
        if !enclosed && self.leads_with_episode(begin) {
            return;
        }
        let len = self.tape.len();
        let mut end = (begin..len)
            .find(|&index| {
                self.tape.tokens[index].is_taken()
                    || (enclosed && self.tape.tokens[index].is_bracket())
            })
            .unwrap_or(len);
        if enclosed {
            self.build_and_insert(ElementKind::AnimeTitle, begin, end, false);
            return;
        }

        let mut last_bracket = end;
        let mut bracket_open = false;
        for index in begin..end {
            if self.tape.tokens[index].is_bracket() {
                last_bracket = index;
                bracket_open = !bracket_open;
            }
        }
        if bracket_open {
            end = last_bracket;
        }

        // Trailing enclosed groups are cut off, except parenthesized ones,
        // so a title like "Anime (TV)" stays intact.
        let mut last = self.tape.prev(end, token::is_not_delimiter);
        while let Some(index) = last
            && self.tape.tokens[index].is_bracket()
            && !self.tape.tokens[index].text.starts_with(')')
        {
            let Some(opener) = self.tape.prev(index, Token::is_bracket) else {
                break;
            };
            end = opener;
            last = self.tape.prev(end, token::is_not_delimiter);
        }

        self.build_and_insert(ElementKind::AnimeTitle, begin, end, false);
    }

    /// A name that leads with its episode and sets it off with a dash
    /// (`03 - The Hidden Village`, `Ep. 07 - Snow Falls`) has no anime
    /// title: what follows the dash is the episode title. Without the dash
    /// (`Episode 14 Ore no Imouto`) the rest is the anime title as usual.
    fn leads_with_episode(&self, begin: usize) -> bool {
        let is_dash = |index: usize| string::is_dash(&self.tape.tokens[index].text);
        self.episode_token.is_some_and(|episode| episode < begin)
            && (is_dash(begin)
                || self
                    .tape
                    .prev(begin, token::is_not_delimiter)
                    .is_some_and(is_dash))
    }
}
