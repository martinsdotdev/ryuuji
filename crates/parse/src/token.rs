use std::ops::Range;

use crate::element::{ElementKind, Span};
use crate::engine::RuleName;
use crate::string;

/// What a token is before any rule has an opinion about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Shape {
    Open,
    Close,
    Delimiter,
    Word,
    /// A term the tokenizer cut out of a longer run because the keyword
    /// table lists it whole (`H.264`, `Dual Audio`). Never free.
    Term,
}

/// Which rule took which part of a token, and as what. A held taking reads
/// a value but leaves the chars to a later title, which is how an
/// unidentifiable keyword (`Ita`, `END`, `Movie`) stays a title word.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Taking {
    pub(crate) by: RuleName,
    pub(crate) kind: ElementKind,
    /// Byte range of the token's own text.
    pub(crate) part: Range<usize>,
    pub(crate) held: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Token {
    pub(crate) shape: Shape,
    pub(crate) text: String,
    /// Where the token sits in the string the caller passed to
    /// [`crate::parse`].
    pub(crate) span: Span,
    pub(crate) enclosed: bool,
    /// The text is one run of ASCII digits; cached because every number
    /// rule asks.
    pub(crate) numeric: bool,
    /// The kind a pre-identified term maps to.
    pub(crate) term: Option<ElementKind>,
    pub(crate) taken: Vec<Taking>,
}

impl Token {
    pub(crate) fn new(
        shape: Shape,
        text: String,
        span: Span,
        enclosed: bool,
        term: Option<ElementKind>,
    ) -> Token {
        let numeric = string::is_numeric(&text);
        Token {
            shape,
            text,
            span,
            enclosed,
            numeric,
            term,
            taken: Vec::new(),
        }
    }

    pub(crate) fn is_bracket(&self) -> bool {
        matches!(self.shape, Shape::Open | Shape::Close)
    }

    pub(crate) fn is_delimiter(&self) -> bool {
        self.shape == Shape::Delimiter
    }

    /// A word with chars no rule has taken: the working set of every rule.
    pub(crate) fn is_free(&self) -> bool {
        self.shape == Shape::Word && self.free_len() > 0
    }

    /// A word or term some rule has read whole; where a free run ends.
    pub(crate) fn is_taken(&self) -> bool {
        matches!(self.shape, Shape::Word | Shape::Term) && !self.is_free()
    }

    /// The parts of the text hard takings cover, merged and in order.
    fn covered(&self) -> Vec<Range<usize>> {
        let mut parts: Vec<Range<usize>> = self
            .taken
            .iter()
            .filter(|taking| !taking.held)
            .map(|taking| taking.part.clone())
            .collect();
        parts.sort_by_key(|part| part.start);
        let mut merged: Vec<Range<usize>> = Vec::new();
        for part in parts {
            match merged.last_mut() {
                Some(last) if part.start <= last.end => last.end = last.end.max(part.end),
                _ => merged.push(part),
            }
        }
        merged
    }

    fn free_len(&self) -> usize {
        self.text.len()
            - self
                .covered()
                .iter()
                .map(|part| part.end - part.start)
                .sum::<usize>()
    }

    /// The text no rule has taken, which is what a title may still keep.
    pub(crate) fn free_text(&self) -> String {
        let mut free = String::new();
        let mut cursor = 0;
        for part in self.covered() {
            free.push_str(&self.text[cursor..part.start]);
            cursor = part.end;
        }
        free.push_str(&self.text[cursor..]);
        free
    }

    pub(crate) fn take(&mut self, by: RuleName, kind: ElementKind, part: Range<usize>, held: bool) {
        self.taken.push(Taking {
            by,
            kind,
            part,
            held,
        });
    }
}

pub(crate) fn is_not_delimiter(token: &Token) -> bool {
    token.shape != Shape::Delimiter
}

/// Whether a value keeps the delimiters between its tokens. A title folds
/// them to spaces; a release group keeps them as written (`Foo_Bar`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Delimiters {
    Folded,
    Kept,
}

/// The token list a rule reads.
#[derive(Debug)]
pub(crate) struct Tape {
    pub(crate) tokens: Vec<Token>,
}

impl Tape {
    pub(crate) fn new(tokens: Vec<Token>) -> Tape {
        Tape { tokens }
    }

    pub(crate) fn len(&self) -> usize {
        self.tokens.len()
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (usize, &Token)> {
        self.tokens.iter().enumerate()
    }

    /// Every token no rule has taken, with its index.
    pub(crate) fn free(&self) -> impl Iterator<Item = (usize, &Token)> {
        self.iter().filter(|(_, token)| token.is_free())
    }

    pub(crate) fn next(&self, from: usize, want: impl Fn(&Token) -> bool) -> Option<usize> {
        (from + 1..self.tokens.len()).find(|&index| want(&self.tokens[index]))
    }

    pub(crate) fn prev(&self, from: usize, want: impl Fn(&Token) -> bool) -> Option<usize> {
        (0..from).rev().find(|&index| want(&self.tokens[index]))
    }

    /// Each run of free tokens carrying the given `enclosed` flag, ending
    /// at the next bracket, taken token, or the end of the tape.
    pub(crate) fn free_runs(&self, enclosed: bool) -> impl Iterator<Item = Range<usize>> + '_ {
        let len = self.tokens.len();
        let mut search_from = 0;
        std::iter::from_fn(move || {
            let begin = (search_from..len).find(|&index| {
                self.tokens[index].enclosed == enclosed && self.tokens[index].is_free()
            })?;
            let end = (begin..len)
                .find(|&index| self.tokens[index].is_bracket() || self.tokens[index].is_taken())
                .unwrap_or(len);
            search_from = end;
            Some(begin..end)
        })
    }

    /// A token alone between two brackets: `[12]`.
    pub(crate) fn isolated(&self, at: usize) -> bool {
        let is_bracket =
            |index: Option<usize>| index.is_some_and(|index| self.tokens[index].is_bracket());
        is_bracket(self.prev(at, is_not_delimiter)) && is_bracket(self.next(at, is_not_delimiter))
    }

    /// The value a run of tokens spells, skipping every taken part.
    pub(crate) fn value(&self, run: Range<usize>, delimiters: Delimiters) -> String {
        let mut value = String::new();
        for token in &self.tokens[run] {
            match token.shape {
                Shape::Word => value.push_str(&token.free_text()),
                Shape::Open | Shape::Close => value.push_str(&token.text),
                Shape::Delimiter => match delimiters {
                    Delimiters::Kept => value.push_str(&token.text),
                    Delimiters::Folded => {
                        if let Some(delimiter) = token.text.chars().next() {
                            value.push(match delimiter {
                                ',' | '&' => delimiter,
                                _ => ' ',
                            });
                        }
                    }
                },
                Shape::Term => {}
            }
        }
        match delimiters {
            Delimiters::Kept => value,
            Delimiters::Folded => string::trim_dashes_and_spaces(&value).to_owned(),
        }
    }

    /// Takes a whole token.
    pub(crate) fn take(&mut self, at: usize, by: RuleName, kind: ElementKind, held: bool) {
        let len = self.tokens[at].text.len();
        self.tokens[at].take(by, kind, 0..len, held);
    }

    /// Cuts one token in two at a byte offset of its text; the second half
    /// lands at `at + 1`. Neither half keeps a taking.
    pub(crate) fn split(&mut self, at: usize, byte: usize) {
        let token = &self.tokens[at];
        let split = token.span.start + byte;
        let head = Token::new(
            token.shape,
            token.text[..byte].to_owned(),
            Span {
                start: token.span.start,
                end: split,
            },
            token.enclosed,
            None,
        );
        let tail = Token::new(
            token.shape,
            token.text[byte..].to_owned(),
            Span {
                start: split,
                end: token.span.end,
            },
            token.enclosed,
            None,
        );
        self.tokens[at] = tail;
        self.tokens.insert(at, head);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(shape: Shape, text: &str) -> Token {
        Token::new(
            shape,
            text.to_owned(),
            Span { start: 0, end: 0 },
            false,
            None,
        )
    }

    fn tape(tokens: Vec<Token>) -> Tape {
        Tape::new(tokens)
    }

    #[test]
    fn a_held_taking_leaves_the_word_free() {
        let mut word = token(Shape::Word, "Movie");
        word.take(RuleName::Legacy, ElementKind::AnimeType, 0..5, true);
        assert!(word.is_free());
        assert_eq!(word.free_text(), "Movie");
        word.take(RuleName::Legacy, ElementKind::AnimeTitle, 0..5, false);
        assert!(!word.is_free());
        assert!(word.is_taken());
        assert_eq!(word.free_text(), "");
    }

    #[test]
    fn a_part_taking_leaves_the_rest_free() {
        let mut word = token(Shape::Word, "OVA1");
        word.take(RuleName::Legacy, ElementKind::EpisodeNumber, 3..4, false);
        assert!(word.is_free());
        assert_eq!(word.free_text(), "OVA");
        word.take(RuleName::Legacy, ElementKind::AnimeType, 0..3, false);
        assert!(!word.is_free());
    }

    #[test]
    fn a_term_is_never_free() {
        let term = Token::new(
            Shape::Term,
            "H.264".to_owned(),
            Span { start: 0, end: 5 },
            false,
            Some(ElementKind::VideoTerm),
        );
        assert!(!term.is_free());
        assert!(term.is_taken());
    }

    #[test]
    fn value_maps_delimiters_and_trims_the_ends() {
        let mut taken = token(Shape::Word, "720p");
        taken.take(RuleName::Legacy, ElementKind::VideoResolution, 0..4, false);
        let tape = tape(vec![
            token(Shape::Delimiter, " "),
            token(Shape::Word, "\u{2013}"),
            token(Shape::Delimiter, " "),
            token(Shape::Word, "Foo"),
            token(Shape::Delimiter, "&"),
            token(Shape::Word, "Bar"),
            token(Shape::Delimiter, "."),
            token(Shape::Word, "Baz"),
            taken,
            token(Shape::Delimiter, " "),
        ]);
        assert_eq!(tape.value(0..10, Delimiters::Folded), "Foo&Bar Baz");
        assert_eq!(
            tape.value(0..10, Delimiters::Kept),
            " \u{2013} Foo&Bar.Baz "
        );
    }

    #[test]
    fn value_keeps_bracket_text() {
        let tape = tape(vec![
            token(Shape::Word, "Fate"),
            token(Shape::Open, "("),
            token(Shape::Word, "Zero"),
            token(Shape::Close, ")"),
        ]);
        assert_eq!(tape.value(0..4, Delimiters::Folded), "Fate(Zero)");
    }

    #[test]
    fn free_runs_end_at_brackets_and_taken_tokens() {
        let mut tape = tape(vec![
            token(Shape::Open, "["),
            token(Shape::Word, "Group"),
            token(Shape::Close, "]"),
            token(Shape::Delimiter, " "),
            token(Shape::Word, "Show"),
            token(Shape::Delimiter, " "),
            token(Shape::Word, "03"),
            token(Shape::Delimiter, " "),
            token(Shape::Word, "Title"),
        ]);
        for token in &mut tape.tokens[..3] {
            token.enclosed = true;
        }
        tape.take(6, RuleName::Legacy, ElementKind::EpisodeNumber, false);
        assert_eq!(tape.free_runs(false).collect::<Vec<_>>(), [4..6, 8..9]);
        assert_eq!(tape.free_runs(true).collect::<Vec<_>>(), vec![1..2]);
    }

    #[test]
    fn split_cuts_a_word_at_a_byte() {
        let mut tape = tape(vec![Token::new(
            Shape::Word,
            "OVA1".to_owned(),
            Span { start: 10, end: 14 },
            true,
            None,
        )]);
        tape.split(0, 3);
        assert_eq!(tape.tokens[0].text, "OVA");
        assert_eq!(tape.tokens[0].span, Span { start: 10, end: 13 });
        assert_eq!(tape.tokens[1].text, "1");
        assert_eq!(tape.tokens[1].span, Span { start: 13, end: 14 });
        assert!(tape.tokens[1].numeric && tape.tokens[1].enclosed);
    }
}
