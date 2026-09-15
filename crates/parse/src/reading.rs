//! What one name says, and how sure the parser is of it.

use std::ops::RangeInclusive;

use crate::element::{ElementKind, Elements, Fact, Span};
use crate::engine::RuleName;
use crate::options::Options;
use crate::string::leading_number;

/// The evidence behind a value, weakest first, so the minimum over a
/// reading's fields is the reading's certainty.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Certainty {
    /// A reading that displaced another the name also supports; every
    /// guess names what it displaced.
    Guessed,
    /// A shape the corpus settles: a number set off by a dash, a number
    /// alone in brackets, the first bracket group as the release group.
    Shaped,
    /// The name says so: a table term, `Episode 12`, `S02E04`.
    Stated,
}

impl Certainty {
    pub const ALL: [Certainty; 3] = [Certainty::Guessed, Certainty::Shaped, Certainty::Stated];

    pub fn label(self) -> &'static str {
        match self {
            Certainty::Guessed => "guessed",
            Certainty::Shaped => "shaped",
            Certainty::Stated => "stated",
        }
    }

    pub fn from_label(label: &str) -> Option<Certainty> {
        Certainty::ALL
            .into_iter()
            .find(|certainty| certainty.label() == label)
    }
}

/// One field of the typed reading: the value, how sure the parser is of
/// it, and the rule that read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claim<T> {
    pub value: T,
    pub certainty: Certainty,
    pub by: RuleName,
}

/// One way to read a piece of a name. `kind: None` is the text left where
/// it stands, which for an unenclosed word means a word of the anime title.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sense {
    pub kind: Option<ElementKind>,
    pub rule: RuleName,
}

/// A doubt a rule stated, or a value a later rule took back. `Byousoku 5
/// Centimeter` reads `5` as the episode because nothing in the name marks
/// one, and records that `5` is also a word of the title, so Now playing
/// can show the choice instead of a silent guess.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alternative {
    pub span: Span,
    /// The disputed value as the reading holds it: `Ita`, `5`.
    pub text: String,
    pub taken: Sense,
    pub passed: Sense,
}

impl Alternative {
    /// What else the text could have been, as copy for a shell: `a word of
    /// the title`, `the episode title`. A shell already shows what the text
    /// was taken as, so only the passed sense is described.
    pub fn passed_description(&self) -> String {
        describe(self.passed.kind, &self.text)
    }
}

fn describe(kind: Option<ElementKind>, text: &str) -> String {
    match kind {
        None => "a word of the title".to_owned(),
        Some(ElementKind::AnimeTitle) => "the title".to_owned(),
        Some(ElementKind::EpisodeNumber) => format!("episode {text}"),
        Some(ElementKind::AnimeSeason) => format!("season {text}"),
        Some(ElementKind::AnimeYear) => format!("the year {text}"),
        Some(kind) => format!("the {}", kind.label().replace('_', " ")),
    }
}

/// What one name says.
///
/// [`Reading::elements`] is the element list the fixture corpus and the
/// Diagnostics grid read. The typed readers beside it are what matching
/// consumes; each carries the certainty of the rule that read it, and a
/// guessed one is explained by [`Reading::alternatives`]. The readers
/// derive from the element list, so there is one record of what the
/// parser found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reading {
    elements: Elements,
    alternatives: Vec<Alternative>,
    /// An episode was read off a prefix, so a second episode number may be
    /// the same episode under another scheme.
    provisional_episode: bool,
    /// The options this reading was made under; a rule that reads more
    /// than one kind gates each on them.
    options: Options,
}

impl Reading {
    pub(crate) fn new(elements: Elements, options: &Options) -> Reading {
        Reading {
            elements,
            alternatives: Vec::new(),
            provisional_episode: false,
            options: options.clone(),
        }
    }

    pub(crate) fn options(&self) -> &Options {
        &self.options
    }

    pub fn elements(&self) -> &Elements {
        &self.elements
    }

    pub(crate) fn elements_mut(&mut self) -> &mut Elements {
        &mut self.elements
    }

    pub(crate) fn push_alternative(&mut self, alternative: Alternative) {
        self.alternatives.push(alternative);
    }

    pub(crate) fn provisional_episode(&self) -> bool {
        self.provisional_episode
    }

    pub(crate) fn set_provisional_episode(&mut self) {
        self.provisional_episode = true;
    }

    pub fn alternatives(&self) -> &[Alternative] {
        &self.alternatives
    }

    pub fn title(&self) -> Option<Claim<&str>> {
        self.elements.fact(ElementKind::AnimeTitle).map(text_claim)
    }

    /// Every episode number the name carries, as the span from lowest to
    /// highest. A batch like `01-02` reads two values in claim order, so the
    /// ends are the min and max rather than the first and last; a single
    /// episode is a range of one. `EpisodeNumberAlt` is one episode under a
    /// second numbering scheme and never widens the range. The certainty is
    /// the weakest among the values.
    pub fn episodes(&self) -> Option<Claim<RangeInclusive<u32>>> {
        let facts: Vec<&Fact> = self
            .elements
            .facts()
            .iter()
            .filter(|fact| fact.kind == ElementKind::EpisodeNumber)
            .collect();
        let mut numbers = facts.iter().filter_map(|fact| leading_number(&fact.value));
        let first = numbers.next()?;
        let (low, high) = numbers.fold((first, first), |(low, high), n| (low.min(n), high.max(n)));
        Some(Claim {
            value: low..=high,
            certainty: facts
                .iter()
                .map(|fact| fact.certainty)
                .min()
                .unwrap_or(Certainty::Stated),
            by: facts[0].by,
        })
    }

    /// The leading number of the first season value.
    pub fn season(&self) -> Option<Claim<u32>> {
        let fact = self.elements.fact(ElementKind::AnimeSeason)?;
        Some(Claim {
            value: leading_number(&fact.value)?,
            certainty: fact.certainty,
            by: fact.by,
        })
    }

    pub fn release_group(&self) -> Option<Claim<&str>> {
        self.elements
            .fact(ElementKind::ReleaseGroup)
            .map(text_claim)
    }

    /// The weakest certainty among the typed readers that found a value,
    /// so one guess makes the whole reading a guess. `None` when none did.
    pub fn certainty(&self) -> Option<Certainty> {
        [
            self.title().map(|claim| claim.certainty),
            self.episodes().map(|claim| claim.certainty),
            self.season().map(|claim| claim.certainty),
            self.release_group().map(|claim| claim.certainty),
        ]
        .into_iter()
        .flatten()
        .min()
    }
}

fn text_claim(fact: &Fact) -> Claim<&str> {
    Claim {
        value: fact.value.as_str(),
        certainty: fact.certainty,
        by: fact.by,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::fact;

    fn reading(items: &[(ElementKind, &str)]) -> Reading {
        let mut elements = Elements::default();
        for (kind, value) in items {
            elements.push(fact(*kind, value));
        }
        Reading::new(elements, &Options::default())
    }

    #[test]
    fn certainty_orders_weakest_first() {
        assert!(Certainty::Guessed < Certainty::Shaped);
        assert!(Certainty::Shaped < Certainty::Stated);
        assert_eq!(Certainty::ALL.iter().min(), Some(&Certainty::Guessed));
    }

    #[test]
    fn labels_round_trip_for_every_certainty() {
        for certainty in Certainty::ALL {
            assert_eq!(Certainty::from_label(certainty.label()), Some(certainty));
        }
        assert_eq!(Certainty::from_label("sure"), None);
    }

    #[test]
    fn episodes_of_a_single_value_is_one_wide() {
        let reading = reading(&[(ElementKind::EpisodeNumber, "03")]);
        assert_eq!(reading.episodes().map(|e| e.value), Some(3..=3));
    }

    #[test]
    fn episodes_span_a_batch_whatever_the_claim_order() {
        let reading = reading(&[
            (ElementKind::EpisodeNumber, "12"),
            (ElementKind::EpisodeNumber, "01"),
        ]);
        assert_eq!(reading.episodes().map(|e| e.value), Some(1..=12));
    }

    #[test]
    fn episodes_is_none_without_an_episode_number() {
        assert_eq!(Reading::default().episodes(), None);
        let reading = reading(&[(ElementKind::EpisodeNumberAlt, "08")]);
        assert_eq!(reading.episodes(), None);
    }

    #[test]
    fn episodes_skips_values_without_a_leading_number() {
        let reading = reading(&[
            (ElementKind::EpisodeNumber, "abc"),
            (ElementKind::EpisodeNumber, "4a"),
        ]);
        assert_eq!(reading.episodes().map(|e| e.value), Some(4..=4));
        let only_garbage = self::reading(&[(ElementKind::EpisodeNumber, "v2")]);
        assert_eq!(only_garbage.episodes(), None);
    }

    // `Ep. 08 - 05v2` is one episode under two numbering schemes, not a
    // batch, so the alternate number must not widen the range.
    #[test]
    fn episodes_ignores_the_alternate_number() {
        let reading = reading(&[
            (ElementKind::EpisodeNumber, "05v2"),
            (ElementKind::EpisodeNumberAlt, "08"),
        ]);
        assert_eq!(reading.episodes().map(|e| e.value), Some(5..=5));
    }

    #[test]
    fn season_reads_the_first_anime_season() {
        let reading = reading(&[
            (ElementKind::AnimeSeason, "2"),
            (ElementKind::AnimeSeason, "3"),
        ]);
        assert_eq!(reading.season().map(|s| s.value), Some(2));
    }

    #[test]
    fn certainty_is_the_weakest_reader() {
        let mut elements = Elements::default();
        elements.push(fact(ElementKind::AnimeTitle, "Show"));
        elements.push(Fact {
            certainty: Certainty::Guessed,
            ..fact(ElementKind::EpisodeNumber, "5")
        });
        let reading = Reading::new(elements, &Options::default());
        assert_eq!(
            reading.title().map(|t| t.certainty),
            Some(Certainty::Shaped)
        );
        assert_eq!(reading.certainty(), Some(Certainty::Guessed));
        assert_eq!(Reading::default().certainty(), None);
    }

    #[test]
    fn an_alternative_describes_both_senses() {
        let alternative = Alternative {
            span: Span { start: 9, end: 10 },
            text: "5".to_owned(),
            taken: Sense {
                kind: Some(ElementKind::EpisodeNumber),
                rule: RuleName::Terms,
            },
            passed: Sense {
                kind: None,
                rule: RuleName::Terms,
            },
        };
        assert_eq!(alternative.passed_description(), "a word of the title");
    }
}
