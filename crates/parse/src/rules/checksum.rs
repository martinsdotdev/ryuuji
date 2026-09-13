//! Eight hex digits are a CRC32 checksum. The first one wins; a word the
//! keyword table knows is not a checksum whatever its letters.

use crate::element::ElementKind;
use crate::engine::{Rule, RuleName, Verdict};
use crate::keyword::KeywordTable;
use crate::reading::{Certainty, Reading};
use crate::string;
use crate::token::Tape;

pub(crate) const RULE: Rule = Rule {
    name: RuleName::Checksum,
    settles: Some(ElementKind::FileChecksum),
    gate: |_| true,
    certainty: Certainty::Shaped,
    read,
};

fn read(tape: &Tape, _reading: &Reading) -> Verdict {
    let table = KeywordTable::builtin();
    for (at, token) in tape.free() {
        let word = string::trim_dashes_and_spaces(&token.text);
        if word.chars().count() != 8
            || !string::is_hex(word)
            || table.find_searchable(&word.to_uppercase()).is_some()
        {
            continue;
        }
        return Verdict::nothing().take_part(
            ElementKind::FileChecksum,
            at,
            0..token.text.len(),
            word,
        );
    }
    Verdict::nothing()
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
}
