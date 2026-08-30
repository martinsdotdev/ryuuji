use crate::token::{Token, TokenCategory};

pub(crate) fn is_numeric(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| c.is_ascii_digit())
}

pub(crate) fn is_hex(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| c.is_ascii_hexdigit())
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn is_mostly_latin(text: &str) -> bool {
    let total = text.chars().count();
    if total == 0 {
        return false;
    }
    let latin = text.chars().filter(|&c| c <= '\u{024F}').count();
    latin * 2 >= total
}

pub(crate) fn trim_dashes_and_spaces(text: &str) -> &str {
    text.trim_matches(|c: char| c == ' ' || c == '-' || ('\u{2010}'..='\u{2015}').contains(&c))
}

#[cfg_attr(not(test), allow(dead_code))]
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
    fn mostly_latin_needs_at_least_half_latin_chars() {
        assert!(is_mostly_latin("Toradora"));
        assert!(!is_mostly_latin("\u{3068}\u{3089}\u{30c9}\u{30e9}"));
        assert!(is_mostly_latin("ab\u{3068}\u{3089}"));
        assert!(!is_mostly_latin(""));
    }

    #[test]
    fn trim_covers_ascii_and_unicode_dashes() {
        assert_eq!(trim_dashes_and_spaces(" -\u{2014}Title\u{2013} "), "Title");
    }
}
