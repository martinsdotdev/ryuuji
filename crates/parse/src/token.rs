use crate::element::ElementKind;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TokenCategory {
    Unknown,
    Bracket,
    Delimiter,
    Identifier,
    Invalid,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Token {
    pub(crate) category: TokenCategory,
    pub(crate) content: String,
    pub(crate) enclosed: bool,
    /// Set only for pre-identified tokens.
    pub(crate) kind: Option<ElementKind>,
}

pub(crate) fn find_next(
    tokens: &[Token],
    from: usize,
    predicate: impl Fn(&Token) -> bool,
) -> Option<usize> {
    (from + 1..tokens.len()).find(|&index| predicate(&tokens[index]))
}

pub(crate) fn find_prev(
    tokens: &[Token],
    from: usize,
    predicate: impl Fn(&Token) -> bool,
) -> Option<usize> {
    (0..from).rev().find(|&index| predicate(&tokens[index]))
}

pub(crate) fn is_valid(token: &Token) -> bool {
    token.category != TokenCategory::Invalid
}

pub(crate) fn is_not_delimiter(token: &Token) -> bool {
    !matches!(
        token.category,
        TokenCategory::Delimiter | TokenCategory::Invalid
    )
}
