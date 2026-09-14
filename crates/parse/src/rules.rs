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
mod episode_isolated;
mod episode_last;
mod episode_pair;
mod episode_partial;
mod episode_prefix;
mod episode_range;
mod episode_separated;
mod episode_sign;
mod episode_title;
mod episode_type;
mod episode_version;
mod isolated_resolution;
mod legacy;
mod preidentified;
mod release_group;
mod release_group_trailing;
mod resolution;
mod season_episode;
mod season_word;
mod term_in_anime_title;
mod terms;
mod title;
mod volume_in_word;
mod volume_prefix;
mod year;

use crate::element::ElementKind;
use crate::engine::{Step, Verdict, WordRule};
use crate::numbering::{self, Extent, Numbering};
use crate::reading::Reading;
use crate::string;
use crate::token::Tape;

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

/// Where a title that lives inside brackets starts: the first free token of
/// the second bracket group, on the assumption that the first group is the
/// release group. A group that opens with a mostly non-Latin token is
/// skipped as well, so a CJK group name followed by a CJK title still leads
/// to the Latin title behind them.
pub(super) fn enclosed_title_begin(tape: &Tape) -> Option<usize> {
    let len = tape.len();
    let free_from = |from: usize| {
        (from..len).find(|&index| tape.tokens[index].enclosed && tape.tokens[index].is_free())
    };
    let mut begin = free_from(0)?;
    let mut skipped_a_group = false;
    loop {
        if skipped_a_group && string::is_mostly_latin(&tape.tokens[begin].text) {
            return Some(begin);
        }
        let bracket = (begin..len).find(|&index| tape.tokens[index].is_bracket())?;
        begin = free_from(bracket)?;
        skipped_a_group = true;
    }
}

/// The token the first episode number was read from, so the title rule can
/// tell a name that leads with its episode from one that leads with its
/// title. The first number read may since have been retagged as the
/// alternate scheme, so both kinds count.
pub(super) fn episode_token(tape: &Tape, reading: &Reading) -> Option<usize> {
    let fact = reading.elements().facts().iter().find(|fact| {
        matches!(
            fact.kind,
            ElementKind::EpisodeNumber | ElementKind::EpisodeNumberAlt
        )
    })?;
    tape.iter()
        .find(|(_, token)| token.span.start <= fact.span.start && fact.span.start < token.span.end)
        .map(|(at, _)| at)
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
    Step::Tape(episode_isolated::RULE),
    Step::Tape(episode_last::RULE),
    Step::Tape(title::RULE),
    Step::Tape(release_group::RULE),
    Step::Tape(release_group_trailing::RULE),
    Step::Tape(episode_title::RULE),
    Step::Tape(term_in_anime_title::RULE),
    legacy::STEP,
];
