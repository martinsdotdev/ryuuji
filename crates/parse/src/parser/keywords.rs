use super::Parser;
use crate::element::ElementKind;
use crate::string;

impl Parser<'_> {
    pub(super) fn search_keywords(&mut self) {
        for index in 0..self.tape.len() {
            if !self.tape.tokens[index].is_free() {
                continue;
            }

            let word = string::trim_dashes_and_spaces(&self.tape.tokens[index].text).to_owned();
            if word.is_empty() {
                continue;
            }
            if word.chars().count() != 8 && string::is_numeric(&word) {
                continue;
            }
            let upper = word.to_uppercase();

            if self.table.find_searchable(&upper).is_some()
                || self.elements.contains(ElementKind::VideoResolution)
                || !is_resolution(&word)
            {
                continue;
            }
            self.record(ElementKind::VideoResolution, word, index);
            self.retire(index, ElementKind::VideoResolution);
        }
    }
}

fn is_resolution(word: &str) -> bool {
    fn digit_run(chars: &[char]) -> usize {
        chars.iter().take_while(|c| c.is_ascii_digit()).count()
    }
    let chars: Vec<char> = word.chars().collect();
    let width = digit_run(&chars);
    if !(3..=4).contains(&width) {
        return false;
    }
    match chars.get(width) {
        Some('x' | 'X' | '×') => {
            let rest = &chars[width + 1..];
            let height = digit_run(rest);
            (3..=4).contains(&height) && height == rest.len()
        }
        Some('p' | 'P' | 'i' | 'I') => chars.len() == width + 1,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolutions_match_dimensions_and_scanlines() {
        for word in ["1280x720", "1920X1080", "640×480", "720p", "1080P", "1080i"] {
            assert!(is_resolution(word), "{word}");
        }
        for word in ["10x10", "720", "720px", "x264", "1080pp"] {
            assert!(!is_resolution(word), "{word}");
        }
    }
}
