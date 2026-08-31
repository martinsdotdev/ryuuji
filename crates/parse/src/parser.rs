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
