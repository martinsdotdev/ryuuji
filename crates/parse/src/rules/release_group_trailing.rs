//! Scene-style naming hangs the group off the end after a dash
//! (`...(BDrip 1920x1080 x264)-ank.mkv`); the dash glues onto the word, so
//! the last token outside the brackets reads `-ank`.

use crate::element::ElementKind;
use crate::engine::{Rule, RuleName, Verdict};
use crate::reading::{Certainty, Reading};
use crate::token::{self, Tape};

pub(crate) const RULE: Rule = Rule {
    name: RuleName::ReleaseGroupTrailing,
    settles: Some(ElementKind::ReleaseGroup),
    gate: |options| options.parse_release_group,
    certainty: Certainty::Shaped,
    read,
};

fn read(tape: &Tape, _reading: &Reading) -> Verdict {
    let Some(last) = tape.prev(tape.len(), token::is_not_delimiter) else {
        return Verdict::nothing();
    };
    let token = &tape.tokens[last];
    if token.enclosed || !token.is_free() {
        return Verdict::nothing();
    }
    let Some(group) = token.text.strip_prefix('-') else {
        return Verdict::nothing();
    };
    if group.is_empty() || group.contains('-') {
        return Verdict::nothing();
    }
    let after_bracket = tape
        .prev(last, token::is_not_delimiter)
        .is_some_and(|prev| tape.tokens[prev].is_bracket());
    if !after_bracket {
        return Verdict::nothing();
    }
    Verdict::nothing().take_part(ElementKind::ReleaseGroup, last, 0..token.text.len(), group)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::probe;

    #[test]
    fn a_dashed_word_after_the_last_bracket_is_the_group() {
        assert_eq!(
            probe(
                RuleName::ReleaseGroupTrailing,
                "Show - 03 (BDrip 1920x1080 x264)-ank.mkv"
            ),
            [(ElementKind::ReleaseGroup, "ank".to_owned())]
        );
    }

    #[test]
    fn a_bracketed_group_settles_the_kind_first() {
        assert!(
            probe(
                RuleName::ReleaseGroupTrailing,
                "[Group] Show - 03 (BDrip)-ank.mkv"
            )
            .is_empty()
        );
    }
}
