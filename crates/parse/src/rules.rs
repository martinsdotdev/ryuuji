//! The roster and its order.
//!
//! A new rule is a file beside this one and a row in [`RULES`]; nothing
//! inside another rule changes. The order is the engine's precedence, so
//! moving a row is how a rule's priority changes, and the diff says so.

mod checksum;
mod episode_prefix;
mod isolated_resolution;
mod legacy;
mod preidentified;
mod resolution;
mod season_word;
mod terms;
mod volume_prefix;
mod year;

use crate::engine::{Step, Verdict};
use crate::numbering::{self, Extent};

/// Reads the number at `at` through the word shapes, or takes it whole.
/// Every piece is taken as the bytes it came from, and the bytes the shape
/// used up are spent so no stray letter is left for a title.
pub(super) fn take_number(verdict: Verdict, at: usize, word: &str, extent: Extent) -> Verdict {
    let Some(numbering) = numbering::read(word, extent) else {
        return verdict.take(extent.number(), at);
    };
    let mut verdict = verdict;
    for piece in numbering.pieces {
        verdict = if piece.held {
            verdict.hold_part(piece.kind, at, piece.part.at, piece.part.text)
        } else {
            verdict.take_part(piece.kind, at, piece.part.at, piece.part.text)
        };
    }
    verdict.spend_part(extent.number(), at, numbering.used)
}

pub(crate) const RULES: &[Step] = &[
    Step::Tape(preidentified::RULE),
    Step::Tape(terms::RULE),
    Step::Tape(season_word::RULE),
    Step::Tape(episode_prefix::RULE),
    Step::Tape(volume_prefix::RULE),
    Step::Tape(checksum::RULE),
    Step::Tape(resolution::RULE),
    Step::Tape(year::RULE),
    Step::Tape(isolated_resolution::RULE),
    legacy::STEP,
];
