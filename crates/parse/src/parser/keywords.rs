use super::Parser;
use crate::element::ElementKind;
use crate::string;
use crate::token::{self, TokenCategory};

impl Parser<'_> {
    pub(super) fn search_keywords(&mut self) {
        for index in 0..self.tokens.len() {
            match self.tokens[index].category {
                TokenCategory::Unknown => {}
                TokenCategory::Identifier => {
                    if let Some(kind) = self.tokens[index].kind {
                        let value = self.tokens[index].content.clone();
                        self.elements.insert(kind, value);
                    }
                    continue;
                }
                _ => continue,
            }

            let mut word = string::trim_dashes_and_spaces(&self.tokens[index].content).to_owned();
            if word.is_empty() {
                continue;
            }
            if word.chars().count() != 8 && string::is_numeric(&word) {
                continue;
            }
            let upper = word.to_uppercase();

            let mut kind = None;
            let mut identifiable = true;
            if let Some(keyword) = self.table.find_any(&upper) {
                if keyword.kind == ElementKind::ReleaseGroup && !self.options.parse_release_group {
                    continue;
                }
                if !keyword.kind.is_searchable() || !keyword.searchable {
                    continue;
                }
                if !keyword.kind.is_multi_valued() && self.elements.contains(keyword.kind) {
                    continue;
                }
                match keyword.kind {
                    ElementKind::AnimeSeasonPrefix => {
                        self.check_anime_season(index);
                        continue;
                    }
                    ElementKind::EpisodePrefix => {
                        if keyword.valid {
                            self.check_extent(index, ElementKind::EpisodeNumber);
                        }
                        continue;
                    }
                    ElementKind::VolumePrefix => {
                        self.check_extent(index, ElementKind::VolumeNumber);
                        continue;
                    }
                    ElementKind::ReleaseVersion => {
                        word.remove(0);
                    }
                    _ => {}
                }
                kind = Some(keyword.kind);
                identifiable = keyword.identifiable;
            } else if !self.elements.contains(ElementKind::FileChecksum)
                && word.chars().count() == 8
                && string::is_hex(&word)
            {
                kind = Some(ElementKind::FileChecksum);
            } else if !self.elements.contains(ElementKind::VideoResolution) && is_resolution(&word)
            {
                kind = Some(ElementKind::VideoResolution);
            }

            if let Some(kind) = kind {
                self.elements.insert(kind, word);
                if identifiable {
                    self.tokens[index].category = TokenCategory::Identifier;
                }
            }
        }
    }

    fn check_anime_season(&mut self, index: usize) {
        if let Some(prev) = token::find_prev(&self.tokens, index, token::is_not_delimiter)
            && let Some(number) = ordinal_number(&self.tokens[prev].content)
        {
            self.elements.insert(ElementKind::AnimeSeason, number);
            self.tokens[prev].category = TokenCategory::Identifier;
            self.tokens[index].category = TokenCategory::Identifier;
            return;
        }
        if let Some(next) = token::find_next(&self.tokens, index, token::is_not_delimiter)
            && string::is_numeric(&self.tokens[next].content)
        {
            let value = self.tokens[next].content.clone();
            self.elements.insert(ElementKind::AnimeSeason, value);
            self.tokens[index].category = TokenCategory::Identifier;
            self.tokens[next].category = TokenCategory::Identifier;
        }
    }

    fn check_extent(&mut self, index: usize, kind: ElementKind) {
        let Some(next) = token::find_next(&self.tokens, index, token::is_not_delimiter) else {
            return;
        };
        if self.tokens[next].category != TokenCategory::Unknown {
            return;
        }
        if !self.tokens[next]
            .content
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit())
        {
            return;
        }
        let value = self.tokens[next].content.clone();
        self.elements.insert(kind, value);
        self.tokens[index].category = TokenCategory::Identifier;
        self.tokens[next].category = TokenCategory::Identifier;
        if kind == ElementKind::EpisodeNumber {
            self.found_episode_keyword = true;
        }
    }
}

fn ordinal_number(word: &str) -> Option<&'static str> {
    match word.to_uppercase().as_str() {
        "1ST" | "FIRST" => Some("1"),
        "2ND" | "SECOND" => Some("2"),
        "3RD" | "THIRD" => Some("3"),
        "4TH" | "FOURTH" => Some("4"),
        "5TH" | "FIFTH" => Some("5"),
        "6TH" | "SIXTH" => Some("6"),
        "7TH" | "SEVENTH" => Some("7"),
        "8TH" | "EIGHTH" => Some("8"),
        "9TH" | "NINTH" => Some("9"),
        _ => None,
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
        Some('p' | 'P') => chars.len() == width + 1,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolutions_match_dimensions_and_scanlines() {
        for word in ["1280x720", "1920X1080", "640×480", "720p", "1080P"] {
            assert!(is_resolution(word), "{word}");
        }
        for word in ["10x10", "720", "720px", "x264", "1080pp"] {
            assert!(!is_resolution(word), "{word}");
        }
    }
}
