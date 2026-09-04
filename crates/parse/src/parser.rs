mod episode_title;
mod group;
mod keywords;
mod numbers;
mod title;
mod validate;

use crate::element::{ElementKind, Elements};
use crate::keyword::KeywordTable;
use crate::options::Options;
use crate::string;
use crate::token::{Token, TokenCategory};

pub(crate) struct Parser<'a> {
    tokens: Vec<Token>,
    elements: Elements,
    options: &'a Options,
    table: &'a KeywordTable,
    found_episode_keyword: bool,
}

impl<'a> Parser<'a> {
    pub(crate) fn new(
        tokens: Vec<Token>,
        elements: Elements,
        options: &'a Options,
        table: &'a KeywordTable,
    ) -> Parser<'a> {
        Parser {
            tokens,
            elements,
            options,
            table,
            found_episode_keyword: false,
        }
    }

    pub(crate) fn run(mut self) -> Elements {
        self.search_keywords();
        self.search_isolated_numbers();
        if self.options.parse_episode_number {
            self.search_episode_number();
        }
        self.search_anime_title();
        if self.options.parse_release_group {
            self.search_release_group();
        }
        if self.options.parse_episode_title {
            self.search_episode_title();
        }
        self.validate_elements();
        self.elements
    }

    /// Each run of unknown tokens carrying the given `enclosed` flag, ending
    /// at the next bracket, identifier, or the end of the token list.
    fn unknown_spans(&self, enclosed: bool) -> impl Iterator<Item = (usize, usize)> + '_ {
        let len = self.tokens.len();
        let mut search_from = 0;
        std::iter::from_fn(move || {
            let begin = (search_from..len).find(|&index| {
                self.tokens[index].enclosed == enclosed
                    && self.tokens[index].category == TokenCategory::Unknown
            })?;
            let end = (begin..len)
                .find(|&index| {
                    matches!(
                        self.tokens[index].category,
                        TokenCategory::Bracket | TokenCategory::Identifier
                    )
                })
                .unwrap_or(len);
            search_from = end;
            Some((begin, end))
        })
    }

    /// Builds an element from the span, then retires its unknown tokens so
    /// later passes cannot claim them again.
    fn build_and_insert(
        &mut self,
        kind: ElementKind,
        begin: usize,
        end: usize,
        keep_delimiters: bool,
    ) {
        let value = string::build_element(&self.tokens[begin..end], keep_delimiters);
        for token in &mut self.tokens[begin..end] {
            if token.category == TokenCategory::Unknown {
                token.category = TokenCategory::Identifier;
            }
        }
        self.elements.insert(kind, value);
    }
}
