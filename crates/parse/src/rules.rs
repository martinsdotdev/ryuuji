//! The roster and its order.
//!
//! A new rule is a file beside this one and a row in [`RULES`]; nothing
//! inside another rule changes. The order is the engine's precedence, so
//! moving a row is how a rule's priority changes, and the diff says so.

mod legacy;
mod preidentified;
mod season_word;
mod terms;

use crate::engine::Step;

pub(crate) const RULES: &[Step] = &[
    Step::Tape(preidentified::RULE),
    Step::Tape(terms::RULE),
    Step::Tape(season_word::RULE),
    legacy::STEP,
];
