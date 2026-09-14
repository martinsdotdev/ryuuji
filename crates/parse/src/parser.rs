mod episode_title;
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
        }
    }

    pub(crate) fn run(mut self) {
        if self.options.parse_episode_title {
            self.search_episode_title();
        }
        self.validate_elements();
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
