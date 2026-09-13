//! `[720]`: a bare 480, 720 or 1080 alone between brackets is a resolution
//! that dropped its letter.

use crate::element::ElementKind;
use crate::engine::{Rule, RuleName, Verdict};
use crate::reading::{Certainty, Reading};
use crate::string;
use crate::token::Tape;

pub(crate) const RULE: Rule = Rule {
    name: RuleName::IsolatedResolution,
    settles: Some(ElementKind::VideoResolution),
    gate: |_| true,
    certainty: Certainty::Shaped,
    read,
};

fn read(tape: &Tape, _reading: &Reading) -> Verdict {
    for (at, token) in tape.free() {
        if !token.numeric || !tape.isolated(at) {
            continue;
        }
        if string::leading_number(&token.text)
            .is_some_and(|number| matches!(number, 480 | 720 | 1080))
        {
            return Verdict::nothing().take(ElementKind::VideoResolution, at);
        }
    }
    Verdict::nothing()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::probe;

    #[test]
    fn a_bare_scanline_count_between_brackets_is_the_resolution() {
        assert_eq!(
            probe(RuleName::IsolatedResolution, "[Group] Show 04 [1080].mkv"),
            [(ElementKind::VideoResolution, "1080".to_owned())]
        );
    }

    #[test]
    fn any_other_number_is_not() {
        assert!(probe(RuleName::IsolatedResolution, "[Group] Show [04].mkv").is_empty());
        assert!(probe(RuleName::IsolatedResolution, "[Group] Show 720.mkv").is_empty());
    }
}
