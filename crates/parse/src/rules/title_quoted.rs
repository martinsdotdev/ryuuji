//! A series name the name itself quotes. Several channels write the series
//! inside 《》, 『』 or 「」 and leave everything else loose around it: the
//! episode, the language, the channel's own tag.
//!
//! The title rule cannot find those. It prefers whatever stands free outside
//! the brackets, which in such a name is furniture, and when nothing does it
//! falls back to the *second* bracket group on the reasoning that the first
//! is a release group -- which here lands on the channel.
//!
//! A quote standing after the episode is quoting the episode's own title
//! (`第1話「レインボーバレット」`), so only one before it, or one in a name with
//! no episode at all, is the series.
//!
//! 【】 is deliberately not a quote here: channels write their own tag in it
//! as often as a series, and nothing in the name says which.

use crate::element::ElementKind;
use crate::engine::{Rule, RuleName, Verdict};
use crate::reading::{Certainty, Reading};
use crate::rules::episode_token;
use crate::token::{Delimiters, Tape, Token};

/// The openers that quote a name rather than set a tag aside.
const QUOTES: [&str; 3] = ["《", "『", "「"];

pub(crate) const RULE: Rule = Rule {
    name: RuleName::TitleQuoted,
    settles: Some(ElementKind::AnimeTitle),
    gate: |_| true,
    certainty: Certainty::Shaped,
    read,
};

fn read(tape: &Tape, reading: &Reading) -> Verdict {
    let episode = episode_token(tape, reading);
    let opener = tape
        .iter()
        .find(|(_, token)| QUOTES.contains(&token.text.as_str()))
        .map(|(at, _)| at);
    let Some(open) = opener else {
        return Verdict::nothing();
    };
    if episode.is_some_and(|episode| episode < open) {
        return Verdict::nothing();
    }
    let Some(close) = tape.next(open, Token::is_bracket) else {
        return Verdict::nothing();
    };
    let Some(begin) = (open + 1..close).find(|&at| tape.tokens[at].is_free()) else {
        return Verdict::nothing();
    };
    Verdict::nothing().take_run(ElementKind::AnimeTitle, begin..close, Delimiters::Folded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::probe;

    fn title(input: &str) -> Vec<(ElementKind, String)> {
        probe(RuleName::TitleQuoted, input)
    }

    fn read_title(input: &str) -> Option<String> {
        title(input).into_iter().next().map(|(_, value)| value)
    }

    #[test]
    fn a_quoted_name_before_the_episode_is_the_series() {
        assert_eq!(
            read_title("《幼女戰記 2》#11 (繁中字幕 | 日語原聲)【Ani-One Asia】"),
            Some("幼女戰記 2".to_owned())
        );
        assert_eq!(
            read_title("『無職転生Ⅲ』第13話「日記」"),
            Some("無職転生Ⅲ".to_owned())
        );
        assert_eq!(
            read_title("アニメ「リラックマ」#25"),
            Some("リラックマ".to_owned())
        );
    }

    #[test]
    fn a_quoted_name_after_the_episode_is_an_episode_title() {
        assert!(title("BIRDIE WING 第1話「レインボーバレット」").is_empty());
    }

    #[test]
    fn a_quoted_name_with_no_episode_is_still_the_series() {
        assert_eq!(read_title("《Gachiakuta》"), Some("Gachiakuta".to_owned()));
    }

    #[test]
    fn a_tag_bracket_is_not_a_quote() {
        assert!(title("【Ani-One Asia】").is_empty());
        assert!(title("[Group] Show - 03.mkv").is_empty());
    }

    #[test]
    fn an_empty_quote_reads_nothing() {
        assert!(title("《》 #11").is_empty());
    }
}
