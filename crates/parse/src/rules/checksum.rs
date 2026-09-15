//! Eight hex digits are a CRC32 checksum. The first one wins; a word the
//! keyword table knows is not a checksum whatever its letters.

use crate::element::ElementKind;
use crate::engine::{RuleName, Verdict, WordRule};
use crate::reading::{Certainty, Reading};
use crate::rules::keyword_at;
use crate::string;
use crate::token::Tape;

pub(crate) const RULE: WordRule = WordRule {
    name: RuleName::Checksum,
    certainty: Certainty::Shaped,
    read,
};

fn read(tape: &Tape, reading: &Reading, at: usize) -> Verdict {
    if reading.elements().contains(ElementKind::FileChecksum) {
        return Verdict::nothing();
    }
    let text = &tape.tokens[at].text;
    let word = string::trim_dashes_and_spaces(text);
    if word.chars().count() != 8 || !string::is_hex(word) || keyword_at(tape, at).is_some() {
        return Verdict::nothing();
    }
    Verdict::nothing().take_part(ElementKind::FileChecksum, at, 0..text.len(), word)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::probe;

    #[test]
    fn eight_hex_digits_are_the_checksum() {
        assert_eq!(
            probe(RuleName::Checksum, "[Taka]_Show_04_[720p][40F2A957].mp4"),
            [(ElementKind::FileChecksum, "40F2A957".to_owned())]
        );
        assert_eq!(
            probe(RuleName::Checksum, "Show 04 [12345678].mkv"),
            [(ElementKind::FileChecksum, "12345678".to_owned())]
        );
    }

    #[test]
    fn a_shorter_or_wider_run_is_not() {
        assert!(probe(RuleName::Checksum, "Show 04 [40F2A95].mkv").is_empty());
        assert!(probe(RuleName::Checksum, "Show 04 [40F2A957B].mkv").is_empty());
    }

    #[test]
    fn the_first_checksum_wins() {
        assert_eq!(
            probe(RuleName::Checksum, "Show 04 [FFFFFFFF] [DEADBEEF].mkv"),
            [(ElementKind::FileChecksum, "FFFFFFFF".to_owned())]
        );
    }
}
