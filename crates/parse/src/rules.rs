//! The roster and its order.
//!
//! A new rule is a file beside this one and a row in [`RULES`]; nothing
//! inside another rule changes. The order is the engine's precedence, so
//! moving a row is how a rule's priority changes, and the diff says so.

mod checksum;
mod episode_counter;
mod episode_equivalent;
mod episode_fraction;
mod episode_in_word;
mod episode_pair;
mod episode_partial;
mod episode_prefix;
mod episode_range;
mod episode_separated;
mod episode_sign;
mod episode_type;
mod episode_version;
mod isolated_resolution;
mod legacy;
mod preidentified;
mod resolution;
mod season_episode;
mod season_word;
mod terms;
mod volume_in_word;
mod volume_prefix;
mod year;

use crate::element::ElementKind;
use crate::engine::{Step, Verdict, WordRule};
use crate::numbering::{self, Extent, Numbering};

/// Reads the number at `at` through the word shapes, or takes it whole.
pub(super) fn take_number(verdict: Verdict, at: usize, word: &str, extent: Extent) -> Verdict {
    take_number_from(verdict, at, word, 0, extent)
}

/// Reads the number that starts at byte `from` of the word at `at` through
/// the word shapes, or takes that part whole.
pub(super) fn take_number_from(
    verdict: Verdict,
    at: usize,
    word: &str,
    from: usize,
    extent: Extent,
) -> Verdict {
    match numbering::read(&word[from..], extent) {
        Some(numbering) => take_pieces(verdict, at, numbering.shift(from), extent),
        None => verdict.take_part(extent.number(), at, from..word.len(), &word[from..]),
    }
}

/// Every piece is taken as the bytes it came from, and the bytes the shape
/// used up are spent so no stray letter is left for a title.
pub(super) fn take_pieces(
    verdict: Verdict,
    at: usize,
    numbering: Numbering,
    extent: Extent,
) -> Verdict {
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

/// The word shapes, tried on each free word in turn until one reads the
/// episode. A volume read on the way applies and the walk goes on.
const WORDS: &[WordRule] = &[
    episode_in_word::RULE,
    volume_in_word::RULE,
    episode_pair::RULE,
    episode_version::RULE,
    episode_range::RULE,
    season_episode::RULE,
    episode_type::RULE,
    episode_fraction::RULE,
    episode_partial::RULE,
    episode_sign::RULE,
    episode_counter::RULE,
];

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
    Step::Words {
        gate: |options| options.parse_episode_number,
        rules: WORDS,
        until: ElementKind::EpisodeNumber,
    },
    Step::Tape(episode_equivalent::RULE),
    Step::Tape(episode_separated::RULE),
    legacy::STEP,
];
