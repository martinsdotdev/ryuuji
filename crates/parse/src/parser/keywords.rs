use super::Parser;
use crate::element::ElementKind;
use crate::numbering::Extent;
use crate::string;
use crate::token;

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

            let kind;
            if let Some(keyword) = self.table.find_searchable(&upper) {
                match keyword.kind {
                    ElementKind::EpisodePrefix => {
                        if keyword.valid {
                            self.check_extent(index, Extent::Episode);
                        }
                    }
                    ElementKind::VolumePrefix => self.check_extent(index, Extent::Volume),
                    _ => {}
                }
                continue;
            } else if !self.elements.contains(ElementKind::FileChecksum)
                && word.chars().count() == 8
                && string::is_hex(&word)
            {
                kind = ElementKind::FileChecksum;
            } else if !self.elements.contains(ElementKind::VideoResolution) && is_resolution(&word)
            {
                kind = ElementKind::VideoResolution;
            } else {
                continue;
            }
            self.record(kind, word, index);
            self.retire(index, kind);
        }
    }

    fn check_extent(&mut self, index: usize, extent: Extent) {
        let Some(next) = self.tape.next(index, token::is_not_delimiter) else {
            return;
        };
        if !self.tape.tokens[next].is_free() {
            return;
        }
        if !self.tape.tokens[next]
            .text
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit())
        {
            return;
        }
        let value = self.tape.tokens[next].text.clone();
        self.claim_number(extent, &value, next);
        self.retire(index, extent.prefix());
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
