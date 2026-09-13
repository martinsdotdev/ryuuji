mod episode_title;
mod group;
mod keywords;
mod numbers;
mod title;
mod validate;

use crate::element::{ElementKind, Elements};
use crate::engine::RuleName;
use crate::keyword::KeywordTable;
use crate::options::Options;
use crate::token::{Delimiters, Tape};

pub(crate) struct Parser<'a> {
    tape: Tape,
    elements: Elements,
    options: &'a Options,
    table: &'a KeywordTable,
    found_episode_keyword: bool,
    /// The token the first episode number was read from, so the title
    /// search can tell a name that leads with its episode from one that
    /// leads with its title.
    episode_token: Option<usize>,
}

impl<'a> Parser<'a> {
    pub(crate) fn new(
        tape: Tape,
        elements: Elements,
        options: &'a Options,
        table: &'a KeywordTable,
    ) -> Parser<'a> {
        Parser {
            tape,
            elements,
            options,
            table,
            found_episode_keyword: false,
            episode_token: None,
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

    /// Takes a whole token so later passes cannot claim it again.
    fn retire(&mut self, index: usize, kind: ElementKind) {
        self.tape.take(index, RuleName::Legacy, kind, false);
    }

    /// Reads a value off a token but leaves it free for a title.
    fn hold(&mut self, index: usize, kind: ElementKind) {
        self.tape.take(index, RuleName::Legacy, kind, true);
    }

    /// Builds an element from the span, then retires its free tokens so
    /// later passes cannot claim them again.
    fn build_and_insert(
        &mut self,
        kind: ElementKind,
        begin: usize,
        end: usize,
        keep_delimiters: bool,
    ) {
        let delimiters = if keep_delimiters {
            Delimiters::Kept
        } else {
            Delimiters::Folded
        };
        let value = self.tape.value(begin..end, delimiters);
        for index in begin..end {
            if self.tape.tokens[index].is_free() {
                self.retire(index, kind);
            }
        }
        self.elements.insert(kind, value);
    }
}
