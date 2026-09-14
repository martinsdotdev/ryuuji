//! The release group: the first free run inside a bracket group that runs
//! to the closing bracket and opens the group, delimiters kept as written
//! (`Foo_Bar`).

use crate::element::ElementKind;
use crate::engine::{Rule, RuleName, Verdict};
use crate::reading::{Certainty, Reading};
use crate::token::{self, Delimiters, Tape};

pub(crate) const RULE: Rule = Rule {
    name: RuleName::ReleaseGroup,
    settles: Some(ElementKind::ReleaseGroup),
    gate: |options| options.parse_release_group,
    certainty: Certainty::Shaped,
    read,
};

fn read(tape: &Tape, _reading: &Reading) -> Verdict {
    let Some(run) = tape.free_runs(true).find(|run| {
        run.end < tape.len()
            && tape.tokens[run.end].is_bracket()
            && tape
                .prev(run.start, token::is_not_delimiter)
                .is_none_or(|prev| tape.tokens[prev].is_bracket())
    }) else {
        return Verdict::nothing();
    };
    Verdict::nothing().take_run(ElementKind::ReleaseGroup, run, Delimiters::Kept)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::probe;

    #[test]
    fn the_first_whole_bracket_group_is_the_release_group() {
        assert_eq!(
            probe(RuleName::ReleaseGroup, "[Foo_Bar] Show - 03 [720p].mkv"),
            [(ElementKind::ReleaseGroup, "Foo_Bar".to_owned())]
        );
    }

    #[test]
    fn a_group_that_a_term_opens_is_not() {
        assert!(probe(RuleName::ReleaseGroup, "Show - 03 [BD Foo].mkv").is_empty());
    }
}
