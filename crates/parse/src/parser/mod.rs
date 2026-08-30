mod keywords;

use crate::element::Elements;
use crate::keyword::KeywordTable;
use crate::options::Options;
use crate::token::Token;

pub(crate) struct Parser<'a> {
    tokens: Vec<Token>,
    elements: Elements,
    options: &'a Options,
    table: &'a KeywordTable,
    #[allow(dead_code)]
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
        self.elements
    }
}
