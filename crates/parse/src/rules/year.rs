//! `(2009)`: a number alone between brackets and within the years anime
//! has aired is the year. The first one wins.

use crate::element::ElementKind;
use crate::engine::{Rule, RuleName, Verdict};
use crate::numbering::{ANIME_YEAR_MAX, ANIME_YEAR_MIN};
use crate::reading::{Certainty, Reading};
use crate::string;
use crate::token::Tape;

pub(crate) const RULE: Rule = Rule {
    name: RuleName::Year,
    settles: Some(ElementKind::AnimeYear),
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
            .is_some_and(|number| (ANIME_YEAR_MIN..=ANIME_YEAR_MAX).contains(&number))
        {
            return Verdict::nothing().take(ElementKind::AnimeYear, at);
        }
    }
    Verdict::nothing()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::probe;

    #[test]
    fn an_isolated_number_in_the_range_is_the_year() {
        assert_eq!(
            probe(RuleName::Year, "[Taka]_Show_(2009)_04_[720p].mp4"),
            [(ElementKind::AnimeYear, "2009".to_owned())]
        );
    }

    #[test]
    fn a_number_outside_brackets_or_the_range_is_not() {
        assert!(probe(RuleName::Year, "Show 2009 04.mkv").is_empty());
        assert!(probe(RuleName::Year, "Show (1899) 04.mkv").is_empty());
    }
}
