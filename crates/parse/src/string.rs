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

/// At least half the characters sit at or below the end of the Latin
/// Extended-B block, the cheap test anitomy uses to tell a Latin title from
/// a CJK group name. An empty string is not.
pub(crate) fn is_mostly_latin(text: &str) -> bool {
    let total = text.chars().count().max(1);
    let latin = text.chars().filter(|&c| c <= '\u{024F}').count();
    latin * 2 >= total
}

pub(crate) fn trim_dashes_and_spaces(text: &str) -> &str {
    text.trim_matches(|c: char| c == ' ' || is_dash_char(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leading_number_reads_the_digit_run() {
        assert_eq!(leading_number("01v2"), Some(1));
        assert_eq!(leading_number("7.5"), Some(7));
        assert_eq!(leading_number("v2"), None);
        assert_eq!(leading_number(""), None);
        assert_eq!(leading_number("99999999999"), None);
    }

    #[test]
    fn mostly_latin_is_half_or_more_of_the_chars() {
        assert!(is_mostly_latin("Black Bullet"));
        assert!(!is_mostly_latin(""));
        assert!(is_mostly_latin("K-ON!"));
        assert!(!is_mostly_latin("異域字幕組"));
        assert!(!is_mostly_latin("Re:ゼロから"));
    }

    #[test]
    fn trim_covers_ascii_and_unicode_dashes() {
        assert_eq!(trim_dashes_and_spaces(" -\u{2014}Title\u{2013} "), "Title");
    }
}
