mod episode_title;
mod group;
mod keywords;
mod numbers;
mod title;
mod validate;

use crate::element::{ElementKind, Elements, Fact, Span};
use crate::engine::RuleName;
use crate::keyword::KeywordTable;
use crate::options::Options;
use crate::reading::Certainty;
use crate::token::{Delimiters, Tape};

pub(crate) struct Parser<'a> {
    tape: &'a mut Tape,
    elements: &'a mut Elements,
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
        tape: &'a mut Tape,
        elements: &'a mut Elements,
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

    pub(crate) fn run(mut self) {
        self.episode_token = self
            .elements
            .facts()
            .iter()
            .find(|fact| {
                matches!(
                    fact.kind,
                    ElementKind::EpisodeNumber | ElementKind::EpisodeNumberAlt
                )
            })
            .and_then(|fact| {
                self.tape
                    .iter()
                    .find(|(_, token)| {
                        token.span.start <= fact.span.start && fact.span.start < token.span.end
                    })
                    .map(|(index, _)| index)
            });
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
    }

    /// Records a value read off the token at `index`.
    fn record(&mut self, kind: ElementKind, value: impl Into<String>, index: usize) {
        let span = self.tape.tokens[index].span;
        self.record_span(kind, value, span);
    }

    fn record_span(&mut self, kind: ElementKind, value: impl Into<String>, span: Span) {
        self.elements.push(Fact {
            kind,
            value: value.into(),
            span,
            by: RuleName::Legacy,
            certainty: Certainty::Shaped,
        });
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
        let span = self.tape.span(begin..end);
        for index in begin..end {
            if self.tape.tokens[index].is_free() {
                self.retire(index, kind);
            }
        }
        self.record_span(kind, value, span);
    }
}
