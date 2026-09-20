use crate::element::{ElementKind, Span};
use crate::keyword::KeywordTable;
use crate::options::Options;
use crate::string;
use crate::token::{Shape, Token};

const BRACKET_PAIRS: [(char, char); 11] = [
    ('(', ')'),
    ('[', ']'),
    ('{', '}'),
    ('「', '」'),
    ('『', '』'),
    ('【', '】'),
    ('（', '）'),
    ('《', '》'),
    ('〈', '〉'),
    ('〔', '〕'),
    ('“', '”'),
];

/// The characters a site or an uploader writes between the parts of a name:
/// `Series | Episode 12 | Multi Sub`. They are not delimiters, because a
/// delimiter stands inside a name the way a space or an underscore does,
/// and these end one. The full-width bar is its own character rather than a
/// spelling of the ASCII one, and at least one channel writes a
/// box-drawing line where it means a bar.
const SEPARATORS: [char; 4] = ['|', '｜', '│', '•'];

fn matching_closer(open: char) -> Option<char> {
    BRACKET_PAIRS
        .iter()
        .find(|(o, _)| *o == open)
        .map(|(_, close)| *close)
}

/// Splits `input` into tokens whose spans index `input` itself.
pub(crate) fn tokenize(input: &str, options: &Options, table: &KeywordTable) -> Vec<Token> {
    let chars: Vec<char> = input.chars().collect();
    let bytes: Vec<usize> = input
        .char_indices()
        .map(|(offset, _)| offset)
        .chain(std::iter::once(input.len()))
        .collect();
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
                &bytes[start..=end],
                expected_closer.is_some(),
                options,
                &entries,
                &mut tokens,
            );
        }
        let Some(position) = bracket else { break };
        // An opener doubled up (`[[Zero-Raws]`) opens an empty group; the
        // stray first one is dropped so the real group keeps its name.
        if expected_closer.is_none() && chars.get(position + 1) == Some(&chars[position]) {
            start = position + 1;
            continue;
        }
        tokens.push(Token::new(
            match expected_closer {
                None => Shape::Open,
                Some(_) => Shape::Close,
            },
            chars[position].to_string(),
            Span {
                start: bytes[position],
                end: bytes[position + 1],
            },
            true,
            None,
        ));
        expected_closer = match expected_closer {
            None => matching_closer(chars[position]),
            Some(_) => None,
        };
        start = position + 1;
    }
    validate_delimiters(&mut tokens);
    tokens
}

/// `bytes` holds the byte offset of every char in `chars` plus the offset
/// just past the last one, so `bytes[a]..bytes[b]` is the span of
/// `chars[a..b]`.
fn tokenize_group(
    chars: &[char],
    bytes: &[usize],
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
            split_by_delimiters(
                &chars[cursor..start],
                &bytes[cursor..=start],
                enclosed,
                options,
                tokens,
            );
        }
        tokens.push(Token::new(
            Shape::Term,
            chars[start..end].iter().collect(),
            Span {
                start: bytes[start],
                end: bytes[end],
            },
            enclosed,
            Some(kind),
        ));
        cursor = end;
    }
    if cursor < chars.len() {
        split_by_delimiters(
            &chars[cursor..],
            &bytes[cursor..],
            enclosed,
            options,
            tokens,
        );
    }
}

fn split_by_delimiters(
    chars: &[char],
    bytes: &[usize],
    enclosed: bool,
    options: &Options,
    tokens: &mut Vec<Token>,
) {
    let mut delimiters: Vec<char> = Vec::new();
    for &c in chars {
        if !c.is_ascii_alphanumeric()
            && c != '-'
            && !SEPARATORS.contains(&c)
            && options.allowed_delimiters.contains(c)
            && !delimiters.contains(&c)
        {
            delimiters.push(c);
        }
    }
    let push = |tokens: &mut Vec<Token>, shape, from: usize, to: usize| {
        tokens.push(Token::new(
            shape,
            chars[from..to].iter().collect(),
            Span {
                start: bytes[from],
                end: bytes[to],
            },
            enclosed,
            None,
        ));
    };
    let mut start = 0;
    for (index, &c) in chars.iter().enumerate() {
        let shape = if SEPARATORS.contains(&c) {
            Shape::Separator
        } else if delimiters.contains(&c) {
            Shape::Delimiter
        } else {
            continue;
        };
        if index > start {
            push(tokens, Shape::Word, start, index);
        }
        push(tokens, shape, index, index + 1);
        start = index + 1;
    }
    if start < chars.len() {
        push(tokens, Shape::Word, start, chars.len());
    }
}

/// Repairs delimiters that split what reads as one word (`H.265`, `5.1`,
/// `Ep.`, `01+02`) and promotes a delimiter sitting between two of another
/// kind (`_&_`) to a word. A merged-away token dies in place and is dropped
/// at the end, so indices hold throughout.
fn validate_delimiters(tokens: &mut Vec<Token>) {
    fn is_word(token: &Token) -> bool {
        token.shape == Shape::Word
    }
    fn is_single_char(token: &Token) -> bool {
        is_word(token) && token.text.chars().count() == 1 && token.text != "-"
    }
    fn first_char(tokens: &[Token], index: usize) -> char {
        tokens[index].text.chars().next().unwrap_or(' ')
    }
    fn prev_alive(dead: &[bool], from: usize) -> Option<usize> {
        (0..from).rev().find(|&index| !dead[index])
    }
    fn next_alive(dead: &[bool], from: usize) -> Option<usize> {
        (from + 1..dead.len()).find(|&index| !dead[index])
    }
    /// `to` always sits before `from` with nothing alive between them, so
    /// the merged token is contiguous in the input.
    fn append_to(tokens: &mut [Token], dead: &mut [bool], from: usize, to: usize) {
        let text = std::mem::take(&mut tokens[from].text);
        tokens[to].text.push_str(&text);
        tokens[to].span.end = tokens[from].span.end;
        dead[from] = true;
    }

    let mut dead = vec![false; tokens.len()];
    for index in 0..tokens.len() {
        if dead[index] || !tokens[index].is_delimiter() {
            continue;
        }
        let delimiter = first_char(tokens, index);
        let prev = prev_alive(&dead, index);
        let mut next = next_alive(&dead, index);

        if delimiter != ' ' && delimiter != '_' {
            if let Some(prev) = prev.filter(|&prev| is_single_char(&tokens[prev])) {
                append_to(tokens, &mut dead, index, prev);
                while let Some(word) = next.filter(|&next| is_word(&tokens[next])) {
                    append_to(tokens, &mut dead, word, prev);
                    next = next_alive(&dead, word);
                    if let Some(candidate) = next
                        && tokens[candidate].is_delimiter()
                        && first_char(tokens, candidate) == delimiter
                    {
                        append_to(tokens, &mut dead, candidate, prev);
                        next = next_alive(&dead, candidate);
                    }
                }
                continue;
            }
            if let Some(next) = next.filter(|&next| is_single_char(&tokens[next]))
                && let Some(prev) = prev
            {
                append_to(tokens, &mut dead, index, prev);
                append_to(tokens, &mut dead, next, prev);
                continue;
            }
        }

        if let Some(prev) = prev.filter(|&prev| is_word(&tokens[prev]))
            && let Some(next) = next.filter(|&next| tokens[next].is_delimiter())
        {
            let next_delimiter = first_char(tokens, next);
            if delimiter != next_delimiter
                && delimiter != ','
                && (next_delimiter == ' ' || next_delimiter == '_')
            {
                append_to(tokens, &mut dead, index, prev);
                continue;
            }
        } else if let Some(prev) = prev.filter(|&prev| tokens[prev].is_delimiter())
            && let Some(next) = next.filter(|&next| tokens[next].is_delimiter())
        {
            let prev_delimiter = first_char(tokens, prev);
            if prev_delimiter == first_char(tokens, next) && prev_delimiter != delimiter {
                tokens[index].shape = Shape::Word;
            }
        }

        if (delimiter == '&' || delimiter == '+')
            && let Some(prev) = prev.filter(|&prev| is_word(&tokens[prev]))
            && let Some(next) = next.filter(|&next| is_word(&tokens[next]))
            && string::is_numeric(&tokens[prev].text)
            && string::is_numeric(&tokens[next].text)
        {
            append_to(tokens, &mut dead, index, prev);
            append_to(tokens, &mut dead, next, prev);
        }
    }
    let mut index = 0;
    tokens.retain(|_| {
        let keep = !dead[index];
        index += 1;
        keep
    });
    for token in tokens.iter_mut() {
        token.numeric = string::is_numeric(&token.text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(input: &str) -> Vec<Token> {
        tokenize(input, &Options::default(), KeywordTable::builtin())
    }

    fn summary(tokens: &[Token]) -> Vec<(Shape, &str)> {
        tokens
            .iter()
            .map(|token| (token.shape, token.text.as_str()))
            .collect()
    }

    #[test]
    fn bracket_content_is_enclosed_and_the_rest_is_not() {
        let tokens = tokens("[Foo] Bar");
        assert_eq!(
            summary(&tokens),
            [
                (Shape::Open, "["),
                (Shape::Word, "Foo"),
                (Shape::Close, "]"),
                (Shape::Delimiter, " "),
                (Shape::Word, "Bar"),
            ]
        );
        assert_eq!(
            tokens.iter().map(|t| t.enclosed).collect::<Vec<_>>(),
            [true, true, true, false, false]
        );
    }

    #[test]
    fn every_bar_and_bullet_separates() {
        for bar in ["|", "｜", "│", "•"] {
            let input = format!("Frieren {bar} Episode 12");
            assert_eq!(
                summary(&tokens(&input))
                    .into_iter()
                    .map(|(shape, _)| shape)
                    .collect::<Vec<_>>(),
                [
                    Shape::Word,
                    Shape::Delimiter,
                    Shape::Separator,
                    Shape::Delimiter,
                    Shape::Word,
                    Shape::Delimiter,
                    Shape::Word,
                ],
                "{bar}"
            );
        }
    }

    // A bar between two spaces used to be promoted to a word, the way `_&_`
    // is, and then leaked into whatever value the run spelled.
    #[test]
    fn a_spaced_bar_is_not_promoted_to_a_word() {
        assert!(
            !summary(&tokens("Frieren | Episode 12"))
                .iter()
                .any(|&(shape, text)| shape == Shape::Word && text == "|")
        );
    }

    #[test]
    fn a_bar_glued_to_a_word_still_separates() {
        assert_eq!(
            summary(&tokens("[EN Sub]｜Muse PH")).last(),
            Some(&(Shape::Word, "PH"))
        );
        assert!(
            summary(&tokens("[EN Sub]｜Muse PH"))
                .iter()
                .any(|&(shape, _)| shape == Shape::Separator)
        );
    }

    #[test]
    fn spans_index_the_input_in_bytes() {
        let input = "[Fóo]_Bar";
        let spans: Vec<&str> = tokens(input)
            .iter()
            .map(|token| token.span.slice(input))
            .collect();
        assert_eq!(spans, ["[", "Fóo", "]", "_", "Bar"]);
    }

    #[test]
    fn a_merged_token_keeps_one_contiguous_span_and_its_number_flag() {
        let input = "Foo H.265 01+02";
        let tokens = tokens(input);
        let merged = tokens.iter().find(|token| token.text == "H.265").unwrap();
        assert_eq!(merged.span.slice(input), "H.265");
        assert!(!merged.numeric);
        let batch = tokens.iter().find(|token| token.text == "01+02").unwrap();
        assert_eq!(batch.span.slice(input), "01+02");
        assert!(!batch.numeric);
        assert!(
            tokens
                .iter()
                .all(|token| token.numeric == string::is_numeric(&token.text))
        );
    }

    #[test]
    fn a_doubled_opener_is_dropped() {
        assert_eq!(
            summary(&tokens("[[Foo] Bar")),
            [
                (Shape::Open, "["),
                (Shape::Word, "Foo"),
                (Shape::Close, "]"),
                (Shape::Delimiter, " "),
                (Shape::Word, "Bar"),
            ]
        );
    }

    #[test]
    fn delimiters_are_computed_per_group() {
        let tokens = tokens("[Alpha,Beta]_Gamma Delta");
        assert_eq!(
            summary(&tokens),
            [
                (Shape::Open, "["),
                (Shape::Word, "Alpha"),
                (Shape::Delimiter, ","),
                (Shape::Word, "Beta"),
                (Shape::Close, "]"),
                (Shape::Delimiter, "_"),
                (Shape::Word, "Gamma"),
                (Shape::Delimiter, " "),
                (Shape::Word, "Delta"),
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
                (Shape::Word, "Anime"),
                (Shape::Delimiter, " "),
                (Shape::Word, "-"),
                (Shape::Delimiter, " "),
                (Shape::Word, "01"),
            ]
        );
    }

    #[test]
    fn dot_after_short_word_glues_onto_it() {
        assert_eq!(
            summary(&tokens("Ep. 01")),
            [
                (Shape::Word, "Ep."),
                (Shape::Delimiter, " "),
                (Shape::Word, "01"),
            ]
        );
    }

    #[test]
    fn delimiter_between_matching_delimiters_becomes_a_word() {
        assert_eq!(
            summary(&tokens("A_&_B")),
            [
                (Shape::Word, "A"),
                (Shape::Delimiter, "_"),
                (Shape::Word, "&"),
                (Shape::Delimiter, "_"),
                (Shape::Word, "B"),
            ]
        );
    }

    #[test]
    fn plus_between_numbers_merges_them() {
        assert_eq!(summary(&tokens("01+02")), [(Shape::Word, "01+02")]);
    }

    #[test]
    fn single_char_runs_merge_across_the_same_delimiter() {
        assert_eq!(summary(&tokens("H.265")), [(Shape::Word, "H.265")]);
        assert_eq!(summary(&tokens("5.1")), [(Shape::Word, "5.1")]);
    }

    #[test]
    fn preidentified_text_becomes_a_term() {
        let tokens = tokens("H.264");
        assert_eq!(summary(&tokens), [(Shape::Term, "H.264")]);
        assert_eq!(tokens[0].term, Some(ElementKind::VideoTerm));
    }
}
