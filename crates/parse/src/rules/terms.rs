//! Words the keyword table lists: sources, codecs, languages, types, and
//! the rest. A word is looked up trimmed of dashes and spaces, so `-BD-`
//! is `BD`, and a release version drops its `v`. A kind that holds one
//! value per name keeps the first; an unidentifiable term is held rather
//! than taken, so a title can still keep the word.
//!
//! The prefixes that count a number off (`Season`, `Ep.`, `Vol.`) are
//! other rules' business and are skipped here.

use crate::element::ElementKind;
use crate::engine::{Rule, RuleName, Verdict};
use crate::keyword::KeywordTable;
use crate::reading::{Certainty, Reading};
use crate::string;
use crate::token::Tape;

pub(crate) const RULE: Rule = Rule {
    name: RuleName::Terms,
    settles: None,
    gate: |_| true,
    certainty: Certainty::Stated,
    read,
};

fn read(tape: &Tape, reading: &Reading) -> Verdict {
    let table = KeywordTable::builtin();
    let mut verdict = Verdict::nothing();
    let mut read: Vec<ElementKind> = Vec::new();
    for (at, token) in tape.free() {
        let mut word = string::trim_dashes_and_spaces(&token.text).to_owned();
        if word.is_empty() || string::is_numeric(&word) {
            continue;
        }
        let Some(keyword) = table.find_searchable(&word.to_uppercase()) else {
            continue;
        };
        if matches!(
            keyword.kind,
            ElementKind::AnimeSeasonPrefix | ElementKind::EpisodePrefix | ElementKind::VolumePrefix
        ) {
            continue;
        }
        if keyword.kind.is_singular()
            && (reading.elements().contains(keyword.kind) || read.contains(&keyword.kind))
        {
            continue;
        }
        if keyword.kind == ElementKind::ReleaseGroup && !reading.options().parse_release_group {
            continue;
        }
        if keyword.kind == ElementKind::ReleaseVersion {
            word.remove(0);
        }
        read.push(keyword.kind);
        let whole = 0..token.text.len();
        verdict = match (keyword.identifiable, word == token.text) {
            (true, true) => verdict.take(keyword.kind, at),
            (true, false) => verdict.take_part(keyword.kind, at, whole, word),
            (false, true) => verdict.hold(keyword.kind, at),
            (false, false) => verdict.hold_part(keyword.kind, at, whole, word),
        };
    }
    verdict
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::probe;
    use crate::options::Options;

    fn terms(input: &str) -> Vec<(ElementKind, String)> {
        probe(RuleName::Terms, input)
    }

    #[test]
    fn terms_are_read_in_tape_order() {
        assert_eq!(
            terms("[Group] Kara no Kyoukai v2 Batch (BD 1080p FLAC).mkv"),
            [
                (ElementKind::ReleaseVersion, "2".to_owned()),
                (ElementKind::ReleaseInformation, "Batch".to_owned()),
                (ElementKind::Source, "BD".to_owned()),
                (ElementKind::AudioTerm, "FLAC".to_owned()),
            ]
        );
    }

    #[test]
    fn a_singular_kind_keeps_its_first_value() {
        assert_eq!(
            terms("Show v2 v3.mkv"),
            [(ElementKind::ReleaseVersion, "2".to_owned())]
        );
    }

    #[test]
    fn a_word_is_trimmed_before_lookup() {
        assert_eq!(
            terms("Show -BD- 01.mkv"),
            [(ElementKind::Source, "BD".to_owned())]
        );
    }

    #[test]
    fn a_release_group_term_needs_the_option() {
        let read = |options| {
            crate::parse("Show 01 THORA.mkv", &options)
                .elements()
                .get(ElementKind::ReleaseGroup)
                .map(str::to_owned)
        };
        assert_eq!(read(Options::default()), Some("THORA".to_owned()));
        let options = Options {
            parse_release_group: false,
            ..Options::default()
        };
        assert_eq!(read(options), None);
    }

    #[test]
    fn prefixes_are_left_to_their_rules() {
        assert!(terms("Show Season 2 Episode 3 Vol 1.mkv").is_empty());
    }
}
