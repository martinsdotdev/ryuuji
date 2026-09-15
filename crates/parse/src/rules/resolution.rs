//! `1280x720`, `1080p`, `720i`: a video resolution spelled as dimensions or
//! scanlines. The first one wins.

use crate::element::ElementKind;
use crate::engine::{RuleName, Verdict, WordRule};
use crate::reading::{Certainty, Reading};
use crate::rules::keyword_at;
use crate::string;
use crate::token::Tape;

pub(crate) const RULE: WordRule = WordRule {
    name: RuleName::Resolution,
    certainty: Certainty::Shaped,
    read,
};

fn read(tape: &Tape, reading: &Reading, at: usize) -> Verdict {
    if reading.elements().contains(ElementKind::VideoResolution) {
        return Verdict::nothing();
    }
    let text = &tape.tokens[at].text;
    let word = string::trim_dashes_and_spaces(text);
    if !is_resolution(word) || keyword_at(tape, at).is_some() {
        return Verdict::nothing();
    }
    Verdict::nothing().take_part(ElementKind::VideoResolution, at, 0..text.len(), word)
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
    use crate::engine::probe;

    #[test]
    fn resolutions_match_dimensions_and_scanlines() {
        for word in ["1280x720", "1920X1080", "640×480", "720p", "1080P", "1080i"] {
            assert!(is_resolution(word), "{word}");
        }
        for word in ["10x10", "720", "720px", "x264", "1080pp"] {
            assert!(!is_resolution(word), "{word}");
        }
    }

    #[test]
    fn the_first_resolution_wins() {
        assert_eq!(
            probe(RuleName::Resolution, "Show 04 (BD 1440x1080 1080P).mkv"),
            [(ElementKind::VideoResolution, "1440x1080".to_owned())]
        );
    }

    #[test]
    fn a_preidentified_resolution_settles_the_kind_first() {
        assert!(probe(RuleName::Resolution, "Show 04 [720p][1920x1080].mkv").is_empty());
    }
}
