//! The passes not yet lifted into rules of their own, run in place. Each
//! lift moves one pass out of `crate::parser` into a file beside this one
//! and a row above this step; the step is deleted when the last leaves.

use crate::engine::Step;
use crate::keyword::KeywordTable;
use crate::options::Options;
use crate::parser::Parser;
use crate::reading::Reading;
use crate::token::Tape;

pub(crate) const STEP: Step = Step::Legacy(run);

fn run(tape: &mut Tape, reading: &mut Reading, options: &Options) {
    Parser::new(
        tape,
        reading.elements_mut(),
        options,
        KeywordTable::builtin(),
    )
    .run();
}
