use crate::token::{Token, TokenCategory};

pub(crate) fn is_numeric(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| c.is_ascii_digit())
}

pub(crate) fn is_hex(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| c.is_ascii_hexdigit())
}

fn is_dash_char(c: char) -> bool {
    c == '-' || ('\u{2010}'..='\u{2015}').contains(&c)
}

pub(crate) fn is_dash(text: &str) -> bool {
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => is_dash_char(c),
        _ => false,
    }
}

/// The value of the leading digit run, or `None` when there is no run or it
/// does not fit a `u32`.
pub(crate) fn leading_number(text: &str) -> Option<u32> {
    let digits: String = text.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

pub(crate) fn trim_dashes_and_spaces(text: &str) -> &str {
    text.trim_matches(|c: char| c == ' ' || is_dash_char(c))
}

pub(crate) fn build_element(tokens: &[Token], keep_delimiters: bool) -> String {
    let mut element = String::new();
    for token in tokens {
        match token.category {
            TokenCategory::Unknown | TokenCategory::Bracket => element.push_str(&token.content),
            TokenCategory::Delimiter => {
                if keep_delimiters {
                    element.push_str(&token.content);
                } else if let Some(delimiter) = token.content.chars().next() {
                    element.push(match delimiter {
                        ',' | '&' => delimiter,
                        _ => ' ',
                    });
                }
            }
            TokenCategory::Identifier | TokenCategory::Invalid => {}
        }
    }
    if keep_delimiters {
        element
    } else {
        trim_dashes_and_spaces(&element).to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(category: TokenCategory, content: &str) -> Token {
        Token {
            category,
            content: content.to_owned(),
            enclosed: false,
            kind: None,
        }
    }

    #[test]
    fn build_element_maps_delimiters_and_trims_the_ends() {
        let tokens = [
            token(TokenCategory::Delimiter, " "),
            token(TokenCategory::Unknown, "\u{2013}"),
            token(TokenCategory::Delimiter, " "),
            token(TokenCategory::Unknown, "Foo"),
            token(TokenCategory::Delimiter, "&"),
            token(TokenCategory::Unknown, "Bar"),
            token(TokenCategory::Delimiter, "."),
            token(TokenCategory::Unknown, "Baz"),
            token(TokenCategory::Identifier, "720p"),
            token(TokenCategory::Delimiter, " "),
        ];
        assert_eq!(build_element(&tokens, false), "Foo&Bar Baz");
        assert_eq!(build_element(&tokens, true), " \u{2013} Foo&Bar.Baz ");
    }

    #[test]
    fn build_element_keeps_bracket_content() {
        let tokens = [
            token(TokenCategory::Unknown, "Fate"),
            token(TokenCategory::Bracket, "("),
            token(TokenCategory::Unknown, "Zero"),
            token(TokenCategory::Bracket, ")"),
        ];
        assert_eq!(build_element(&tokens, false), "Fate(Zero)");
    }

    #[test]
    fn leading_number_reads_the_digit_run() {
        assert_eq!(leading_number("01v2"), Some(1));
        assert_eq!(leading_number("7.5"), Some(7));
        assert_eq!(leading_number("v2"), None);
        assert_eq!(leading_number(""), None);
        assert_eq!(leading_number("99999999999"), None);
    }

    #[test]
    fn trim_covers_ascii_and_unicode_dashes() {
        assert_eq!(trim_dashes_and_spaces(" -\u{2014}Title\u{2013} "), "Title");
    }
}
