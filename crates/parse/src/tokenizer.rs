use crate::element::ElementKind;
use crate::keyword::KeywordTable;
use crate::options::Options;
use crate::string;
use crate::token::{self, Token, TokenCategory};

const BRACKET_PAIRS: [(char, char); 7] = [
    ('(', ')'),
    ('[', ']'),
    ('{', '}'),
    ('「', '」'),
    ('『', '』'),
    ('【', '】'),
    ('（', '）'),
];

fn matching_closer(open: char) -> Option<char> {
    BRACKET_PAIRS
        .iter()
        .find(|(o, _)| *o == open)
        .map(|(_, close)| *close)
}

pub(crate) fn tokenize(input: &str, options: &Options, table: &KeywordTable) -> Vec<Token> {
    let chars: Vec<char> = input.chars().collect();
    let entries: Vec<(Vec<char>, ElementKind)> = table
        .preidentified()
        .iter()
        .map(|(text, kind)| (text.chars().collect(), *kind))
        .collect();
    let mut tokens = Vec::new();
    let mut expected_closer: Option<char> = None;
    let mut start = 0;
    loop {
        let bracket = chars[start..]
            .iter()
            .position(|&c| match expected_closer {
                None => matching_closer(c).is_some(),
                Some(closer) => c == closer,
            })
            .map(|offset| start + offset);
        let end = bracket.unwrap_or(chars.len());
        if end > start {
            tokenize_group(
                &chars[start..end],
                expected_closer.is_some(),
                options,
                &entries,
                &mut tokens,
            );
        }
        let Some(position) = bracket else { break };
        tokens.push(Token {
            category: TokenCategory::Bracket,
            content: chars[position].to_string(),
            enclosed: true,
            kind: None,
        });
        expected_closer = match expected_closer {
            None => matching_closer(chars[position]),
            Some(_) => None,
        };
        start = position + 1;
    }
    validate_delimiters(&mut tokens);
    tokens
}

fn tokenize_group(
    chars: &[char],
    enclosed: bool,
    options: &Options,
    entries: &[(Vec<char>, ElementKind)],
    tokens: &mut Vec<Token>,
) {
    let mut claims: Vec<(usize, usize, ElementKind)> = Vec::new();
    for (text, kind) in entries {
        let length = text.len();
        if length == 0 || length > chars.len() {
            continue;
        }
        let mut position = 0;
        while position + length <= chars.len() {
            let overlaps = claims
                .iter()
                .any(|&(start, end, _)| position < end && start < position + length);
            if !overlaps && chars[position..position + length] == text[..] {
                claims.push((position, position + length, *kind));
                position += length;
            } else {
                position += 1;
            }
        }
    }
    claims.sort_unstable_by_key(|&(start, _, _)| start);
    let mut cursor = 0;
    for &(start, end, kind) in &claims {
        if start > cursor {
            split_by_delimiters(&chars[cursor..start], enclosed, options, tokens);
        }
        tokens.push(Token {
            category: TokenCategory::Identifier,
            content: chars[start..end].iter().collect(),
            enclosed,
            kind: Some(kind),
        });
        cursor = end;
    }
    if cursor < chars.len() {
        split_by_delimiters(&chars[cursor..], enclosed, options, tokens);
    }
}

fn split_by_delimiters(chars: &[char], enclosed: bool, options: &Options, tokens: &mut Vec<Token>) {
    let mut delimiters: Vec<char> = Vec::new();
    for &c in chars {
        if !c.is_ascii_alphanumeric()
            && c != '-'
            && options.allowed_delimiters.contains(c)
            && !delimiters.contains(&c)
        {
            delimiters.push(c);
        }
    }
    let push = |tokens: &mut Vec<Token>, category, content: String| {
        tokens.push(Token {
            category,
            content,
            enclosed,
            kind: None,
        });
    };
    let mut start = 0;
    for (index, &c) in chars.iter().enumerate() {
        if delimiters.contains(&c) {
            if index > start {
                push(
                    tokens,
                    TokenCategory::Unknown,
                    chars[start..index].iter().collect(),
                );
            }
            push(tokens, TokenCategory::Delimiter, c.to_string());
            start = index + 1;
        }
    }
    if start < chars.len() {
        push(
            tokens,
            TokenCategory::Unknown,
            chars[start..].iter().collect(),
        );
    }
}

fn validate_delimiters(tokens: &mut Vec<Token>) {
    fn is_unknown(token: &Token) -> bool {
        token.category == TokenCategory::Unknown
    }
    fn is_delimiter(token: &Token) -> bool {
        token.category == TokenCategory::Delimiter
    }
    fn is_single_char(token: &Token) -> bool {
        is_unknown(token) && token.content.chars().count() == 1 && token.content != "-"
    }
    fn first_char(tokens: &[Token], index: usize) -> char {
        tokens[index].content.chars().next().unwrap_or(' ')
    }
    fn append_to(tokens: &mut [Token], from: usize, to: usize) {
        let content = std::mem::take(&mut tokens[from].content);
        tokens[to].content.push_str(&content);
        tokens[from].category = TokenCategory::Invalid;
    }

    for index in 0..tokens.len() {
        if tokens[index].category != TokenCategory::Delimiter {
            continue;
        }
        let delimiter = first_char(tokens, index);
        let prev = token::find_prev(tokens, index, token::is_valid);
        let mut next = token::find_next(tokens, index, token::is_valid);

        if delimiter != ' ' && delimiter != '_' {
            if let Some(prev) = prev.filter(|&prev| is_single_char(&tokens[prev])) {
                append_to(tokens, index, prev);
                while let Some(unknown) = next.filter(|&next| is_unknown(&tokens[next])) {
                    append_to(tokens, unknown, prev);
                    next = token::find_next(tokens, unknown, token::is_valid);
                    if let Some(candidate) = next
                        && is_delimiter(&tokens[candidate])
                        && first_char(tokens, candidate) == delimiter
                    {
                        append_to(tokens, candidate, prev);
                        next = token::find_next(tokens, candidate, token::is_valid);
                    }
                }
                continue;
            }
            if let Some(next) = next.filter(|&next| is_single_char(&tokens[next]))
                && let Some(prev) = prev
            {
                append_to(tokens, index, prev);
                append_to(tokens, next, prev);
                continue;
            }
        }

        if let Some(prev) = prev.filter(|&prev| is_unknown(&tokens[prev]))
            && let Some(next) = next.filter(|&next| is_delimiter(&tokens[next]))
        {
            let next_delimiter = first_char(tokens, next);
            if delimiter != next_delimiter
                && delimiter != ','
                && (next_delimiter == ' ' || next_delimiter == '_')
            {
                append_to(tokens, index, prev);
                continue;
            }
        } else if let Some(prev) = prev.filter(|&prev| is_delimiter(&tokens[prev]))
            && let Some(next) = next.filter(|&next| is_delimiter(&tokens[next]))
        {
            let prev_delimiter = first_char(tokens, prev);
            if prev_delimiter == first_char(tokens, next) && prev_delimiter != delimiter {
                tokens[index].category = TokenCategory::Unknown;
            }
        }

        if (delimiter == '&' || delimiter == '+')
            && let Some(prev) = prev.filter(|&prev| is_unknown(&tokens[prev]))
            && let Some(next) = next.filter(|&next| is_unknown(&tokens[next]))
            && string::is_numeric(&tokens[prev].content)
            && string::is_numeric(&tokens[next].content)
        {
            append_to(tokens, index, prev);
            append_to(tokens, next, prev);
        }
    }
    tokens.retain(|token| token.category != TokenCategory::Invalid);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(input: &str) -> Vec<Token> {
        tokenize(input, &Options::default(), KeywordTable::builtin())
    }

    fn summary(tokens: &[Token]) -> Vec<(TokenCategory, &str)> {
        tokens
            .iter()
            .map(|token| (token.category, token.content.as_str()))
            .collect()
    }

    #[test]
    fn bracket_content_is_enclosed_and_the_rest_is_not() {
        let tokens = tokens("[Foo] Bar");
        assert_eq!(
            summary(&tokens),
            [
                (TokenCategory::Bracket, "["),
                (TokenCategory::Unknown, "Foo"),
                (TokenCategory::Bracket, "]"),
                (TokenCategory::Delimiter, " "),
                (TokenCategory::Unknown, "Bar"),
            ]
        );
        assert_eq!(
            tokens.iter().map(|t| t.enclosed).collect::<Vec<_>>(),
            [true, true, true, false, false]
        );
    }

    #[test]
    fn delimiters_are_computed_per_group() {
        let tokens = tokens("[Alpha,Beta]_Gamma Delta");
        assert_eq!(
            summary(&tokens),
            [
                (TokenCategory::Bracket, "["),
                (TokenCategory::Unknown, "Alpha"),
                (TokenCategory::Delimiter, ","),
                (TokenCategory::Unknown, "Beta"),
                (TokenCategory::Bracket, "]"),
                (TokenCategory::Delimiter, "_"),
                (TokenCategory::Unknown, "Gamma"),
                (TokenCategory::Delimiter, " "),
                (TokenCategory::Unknown, "Delta"),
            ]
        );
    }

    #[test]
    fn dash_is_never_a_delimiter() {
        let options = Options {
            allowed_delimiters: " -".to_owned(),
            ..Options::default()
        };
        let tokens = tokenize("Anime - 01", &options, KeywordTable::builtin());
        assert_eq!(
            summary(&tokens),
            [
                (TokenCategory::Unknown, "Anime"),
                (TokenCategory::Delimiter, " "),
                (TokenCategory::Unknown, "-"),
                (TokenCategory::Delimiter, " "),
                (TokenCategory::Unknown, "01"),
            ]
        );
    }

    #[test]
    fn dot_after_short_word_glues_onto_it() {
        assert_eq!(
            summary(&tokens("Ep. 01")),
            [
                (TokenCategory::Unknown, "Ep."),
                (TokenCategory::Delimiter, " "),
                (TokenCategory::Unknown, "01"),
            ]
        );
    }

    #[test]
    fn delimiter_between_matching_delimiters_becomes_unknown() {
        assert_eq!(
            summary(&tokens("A_&_B")),
            [
                (TokenCategory::Unknown, "A"),
                (TokenCategory::Delimiter, "_"),
                (TokenCategory::Unknown, "&"),
                (TokenCategory::Delimiter, "_"),
                (TokenCategory::Unknown, "B"),
            ]
        );
    }

    #[test]
    fn plus_between_numbers_merges_them() {
        assert_eq!(
            summary(&tokens("01+02")),
            [(TokenCategory::Unknown, "01+02")]
        );
    }

    #[test]
    fn single_char_runs_merge_across_the_same_delimiter() {
        assert_eq!(
            summary(&tokens("H.265")),
            [(TokenCategory::Unknown, "H.265")]
        );
        assert_eq!(summary(&tokens("5.1")), [(TokenCategory::Unknown, "5.1")]);
    }

    #[test]
    fn preidentified_text_becomes_an_identifier_token() {
        let tokens = tokens("H.264");
        assert_eq!(summary(&tokens), [(TokenCategory::Identifier, "H.264")]);
        assert_eq!(tokens[0].kind, Some(ElementKind::VideoTerm));
    }
}
