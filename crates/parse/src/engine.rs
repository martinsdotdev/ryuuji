//! The rule engine: the table's row types, a rule's verdict, and the runner
//! that applies verdicts and arbitrates between rules.
//!
//! A rule is pure: it reads the tape and what earlier rules read, and
//! answers with a [`Verdict`]. The engine is the tape's only writer, so a
//! rule test compares verdicts, no rule can see a later rule's work, and
//! no rule can half-apply a decision it then abandons.

#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the verdict ops arrive ahead of the rules that use them"
    )
)]

use std::ops::Range;

use crate::element::{ElementKind, Elements, Fact, Span};
use crate::options::Options;
use crate::reading::{Alternative, Certainty, Reading, Sense};
use crate::rules::RULES;
use crate::string::leading_number;
use crate::token::{Delimiters, Tape};

/// A rule's name: its fixture key, its Diagnostics label, and what a fact
/// reports as its reader. Closed, so a fixture cannot name a rule that does
/// not exist.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RuleName {
    /// The extension strip and the file name, read before any rule runs.
    Prelude,
    Preidentified,
    Terms,
    SeasonWord,
    EpisodePrefix,
    VolumePrefix,
    Checksum,
    Resolution,
    Year,
    IsolatedResolution,
    EpisodeInWord,
    VolumeInWord,
    EpisodePair,
    EpisodeVersion,
    EpisodeRange,
    SeasonEpisode,
    EpisodeType,
    EpisodeFraction,
    EpisodePartial,
    EpisodeSign,
    EpisodeCounter,
    /// The passes not yet lifted into rules of their own.
    Legacy,
}

impl RuleName {
    pub const ALL: [RuleName; 22] = [
        RuleName::Prelude,
        RuleName::Preidentified,
        RuleName::Terms,
        RuleName::SeasonWord,
        RuleName::EpisodePrefix,
        RuleName::VolumePrefix,
        RuleName::Checksum,
        RuleName::Resolution,
        RuleName::Year,
        RuleName::IsolatedResolution,
        RuleName::EpisodeInWord,
        RuleName::VolumeInWord,
        RuleName::EpisodePair,
        RuleName::EpisodeVersion,
        RuleName::EpisodeRange,
        RuleName::SeasonEpisode,
        RuleName::EpisodeType,
        RuleName::EpisodeFraction,
        RuleName::EpisodePartial,
        RuleName::EpisodeSign,
        RuleName::EpisodeCounter,
        RuleName::Legacy,
    ];

    pub fn label(self) -> &'static str {
        match self {
            RuleName::Prelude => "prelude",
            RuleName::Preidentified => "preidentified",
            RuleName::Terms => "terms",
            RuleName::SeasonWord => "season_word",
            RuleName::EpisodePrefix => "episode_prefix",
            RuleName::VolumePrefix => "volume_prefix",
            RuleName::Checksum => "checksum",
            RuleName::Resolution => "resolution",
            RuleName::Year => "year",
            RuleName::IsolatedResolution => "isolated_resolution",
            RuleName::EpisodeInWord => "episode_in_word",
            RuleName::VolumeInWord => "volume_in_word",
            RuleName::EpisodePair => "episode_pair",
            RuleName::EpisodeVersion => "episode_version",
            RuleName::EpisodeRange => "episode_range",
            RuleName::SeasonEpisode => "season_episode",
            RuleName::EpisodeType => "episode_type",
            RuleName::EpisodeFraction => "episode_fraction",
            RuleName::EpisodePartial => "episode_partial",
            RuleName::EpisodeSign => "episode_sign",
            RuleName::EpisodeCounter => "episode_counter",
            RuleName::Legacy => "legacy",
        }
    }

    pub fn from_label(label: &str) -> Option<RuleName> {
        RuleName::ALL.into_iter().find(|name| name.label() == label)
    }
}

/// One rule over the whole tape.
pub(crate) struct Rule {
    pub(crate) name: RuleName,
    /// The kind this rule reads. The engine runs it only while no fact of
    /// that kind stands, which is what the old passes' early returns said.
    /// `None` for a rule that reads many kinds or only retracts.
    pub(crate) settles: Option<ElementKind>,
    pub(crate) gate: fn(&Options) -> bool,
    /// How sure this rule is of everything it reads.
    pub(crate) certainty: Certainty,
    pub(crate) read: fn(&Tape, &Reading) -> Verdict,
}

/// One rule tried on one free word at a time.
pub(crate) struct WordRule {
    pub(crate) name: RuleName,
    pub(crate) certainty: Certainty,
    pub(crate) read: fn(&Tape, &Reading, usize) -> Verdict,
}

/// The scan orders the parser has. A tape rule reads the whole tape once.
/// A word group is tried on each free word in turn, first rule to last,
/// and stops the moment a verdict takes `until`; a verdict that takes
/// something else applies and the walk goes on.
pub(crate) enum Step {
    Tape(Rule),
    Words {
        gate: fn(&Options) -> bool,
        rules: &'static [WordRule],
        until: ElementKind,
    },
    /// Scaffolding: the passes not yet lifted, run in place with write
    /// access. Deleted when the last rule leaves it.
    Legacy(fn(&mut Tape, &mut Reading, &Options)),
}

impl Step {
    fn names(&self) -> Vec<RuleName> {
        match self {
            Step::Tape(rule) => vec![rule.name],
            Step::Words { rules, .. } => rules.iter().map(|rule| rule.name).collect(),
            Step::Legacy(_) => vec![RuleName::Legacy],
        }
    }
}

enum Op {
    /// A fact and a taking. `part` narrows the taking to a byte range of
    /// the token's text and `value` overrides the text as the fact's value.
    Take {
        kind: ElementKind,
        at: usize,
        part: Option<Range<usize>>,
        value: Option<String>,
        held: bool,
    },
    /// A fact spelled by a run of tokens, every free one of which is taken.
    TakeRun {
        kind: ElementKind,
        run: Range<usize>,
        delimiters: Delimiters,
    },
    /// A taking without a fact: the `Ep.` before a number, the dash that
    /// set one off, the letters of `S01E06` around its digits.
    Spend {
        kind: ElementKind,
        at: usize,
        part: Option<Range<usize>>,
    },
    Retract {
        kind: ElementKind,
        value: String,
    },
    /// A doubt: the token at `at` was read as the rule's kind but could be
    /// `passed` instead.
    Instead {
        at: usize,
        passed: Sense,
    },
}

/// A rule's answer: what it read, what it used up, what it doubted.
/// Applied whole or not at all.
#[derive(Default)]
pub(crate) struct Verdict {
    ops: Vec<Op>,
    provisional: bool,
}

impl Verdict {
    /// Nothing to say. The common answer.
    pub(crate) fn nothing() -> Verdict {
        Verdict::default()
    }

    pub(crate) fn is_nothing(&self) -> bool {
        self.ops.is_empty() && !self.provisional
    }

    /// Reads `kind` off one token, value and span the token's own, and
    /// takes the token so no later rule may use it.
    pub(crate) fn take(mut self, kind: ElementKind, at: usize) -> Verdict {
        self.ops.push(Op::Take {
            kind,
            at,
            part: None,
            value: None,
            held: false,
        });
        self
    }

    /// Reads `kind` off part of one token's text with a value of the rule's
    /// choosing: `05v2` is the episode `05` and the version `2`; `OVA1` is
    /// the type `OVA` and the episode `1`, each with its own span, and the
    /// untaken remainder is still a title's.
    pub(crate) fn take_part(
        mut self,
        kind: ElementKind,
        at: usize,
        part: Range<usize>,
        value: impl Into<String>,
    ) -> Verdict {
        self.ops.push(Op::Take {
            kind,
            at,
            part: Some(part),
            value: Some(value.into()),
            held: false,
        });
        self
    }

    /// Reads `kind` off a run of tokens.
    pub(crate) fn take_run(
        mut self,
        kind: ElementKind,
        run: Range<usize>,
        delimiters: Delimiters,
    ) -> Verdict {
        self.ops.push(Op::TakeRun {
            kind,
            run,
            delimiters,
        });
        self
    }

    /// Reads a value and leaves the token free: an unidentifiable term
    /// stays available to a title, and a later rule drops the value if a
    /// title kept the word.
    pub(crate) fn hold(mut self, kind: ElementKind, at: usize) -> Verdict {
        self.ops.push(Op::Take {
            kind,
            at,
            part: None,
            value: None,
            held: true,
        });
        self
    }

    /// Reads a value of the rule's choosing off a token and leaves the
    /// token free.
    pub(crate) fn hold_part(
        mut self,
        kind: ElementKind,
        at: usize,
        part: Range<usize>,
        value: impl Into<String>,
    ) -> Verdict {
        self.ops.push(Op::Take {
            kind,
            at,
            part: Some(part),
            value: Some(value.into()),
            held: true,
        });
        self
    }

    /// Takes a token without reading a value.
    pub(crate) fn spend(mut self, kind: ElementKind, at: usize) -> Verdict {
        self.ops.push(Op::Spend {
            kind,
            at,
            part: None,
        });
        self
    }

    /// Takes part of a token without reading a value.
    pub(crate) fn spend_part(
        mut self,
        kind: ElementKind,
        at: usize,
        part: Range<usize>,
    ) -> Verdict {
        self.ops.push(Op::Spend {
            kind,
            at,
            part: Some(part),
        });
        self
    }

    /// Drops a value an earlier rule read, and records it as what the
    /// text could have been.
    pub(crate) fn retract(mut self, kind: ElementKind, value: impl Into<String>) -> Verdict {
        self.ops.push(Op::Retract {
            kind,
            value: value.into(),
        });
        self
    }

    /// Records the reading this verdict displaced for the token at `at`.
    /// A rule at [`Certainty::Guessed`] is expected to name one.
    pub(crate) fn instead(mut self, at: usize, passed: Sense) -> Verdict {
        self.ops.push(Op::Instead { at, passed });
        self
    }

    /// Marks the episode read here as one of possibly two numbering
    /// schemes; see [`settle_scheme`].
    pub(crate) fn provisional(mut self) -> Verdict {
        self.provisional = true;
        self
    }

    fn takes(&self, kind: ElementKind) -> bool {
        self.ops.iter().any(|op| match op {
            Op::Take { kind: k, held, .. } => *k == kind && !held,
            Op::TakeRun { kind: k, .. } => *k == kind,
            Op::Spend { .. } | Op::Retract { .. } | Op::Instead { .. } => false,
        })
    }
}

/// How the engine settles a second fact of one kind. `Both` stand in claim
/// order (terms, seasons, the episodes of a batch); `First` refuses the
/// verdict that brought the second, which is what the old passes' `contains`
/// guards did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Settle {
    Both,
    First,
}

fn settle(kind: ElementKind) -> Settle {
    if kind.is_singular() {
        Settle::First
    } else {
        Settle::Both
    }
}

/// `Ep. 08 - 05v2` is one episode under two numbering schemes. The rule that
/// reads an episode off a prefix marks the reading provisional, so a later
/// episode does not lose to it: the lower number is the episode and the
/// higher becomes `EpisodeNumberAlt`; an equal number is one reading, not
/// two. Returns the kind the new number lands as, or `None` to drop it.
fn settle_scheme(elements: &mut Elements, new: &str) -> Option<ElementKind> {
    let Some(existing) = elements.get(ElementKind::EpisodeNumber) else {
        return Some(ElementKind::EpisodeNumber);
    };
    let new = leading_number(new).unwrap_or(u32::MAX);
    let old = leading_number(existing).unwrap_or(u32::MAX);
    if new > old {
        Some(ElementKind::EpisodeNumberAlt)
    } else if new < old {
        elements.retag_first(ElementKind::EpisodeNumber, ElementKind::EpisodeNumberAlt);
        Some(ElementKind::EpisodeNumber)
    } else {
        None
    }
}

/// Applies one verdict, or refuses it whole when it brings a second fact
/// of a kind that keeps its first.
fn apply(
    name: RuleName,
    certainty: Certainty,
    verdict: Verdict,
    tape: &mut Tape,
    reading: &mut Reading,
) {
    let refused = verdict.ops.iter().any(|op| {
        let kind = match op {
            Op::Take {
                kind, held: false, ..
            }
            | Op::TakeRun { kind, .. } => *kind,
            _ => return false,
        };
        settle(kind) == Settle::First && reading.elements().contains(kind)
    });
    if refused {
        return;
    }
    for op in verdict.ops {
        match op {
            Op::Take {
                kind,
                at,
                part,
                value,
                held,
            } => {
                let token = &tape.tokens[at];
                let part = part.unwrap_or(0..token.text.len());
                let value = value.unwrap_or_else(|| token.text[part.clone()].to_owned());
                let span = Span {
                    start: token.span.start + part.start,
                    end: token.span.start + part.end,
                };
                let kind = if kind == ElementKind::EpisodeNumber && reading.provisional_episode() {
                    settle_scheme(reading.elements_mut(), &value)
                } else {
                    Some(kind)
                };
                tape.tokens[at].take(name, kind.unwrap_or(ElementKind::EpisodeNumber), part, held);
                if let Some(kind) = kind {
                    reading.elements_mut().push(Fact {
                        kind,
                        value,
                        span,
                        by: name,
                        certainty,
                    });
                }
            }
            Op::TakeRun {
                kind,
                run,
                delimiters,
            } => {
                let value = tape.value(run.clone(), delimiters);
                let span = tape.span(run.clone());
                for at in run {
                    if tape.tokens[at].is_free() {
                        tape.take(at, name, kind, false);
                    }
                }
                reading.elements_mut().push(Fact {
                    kind,
                    value,
                    span,
                    by: name,
                    certainty,
                });
            }
            Op::Spend { kind, at, part } => {
                let part = part.unwrap_or(0..tape.tokens[at].text.len());
                tape.tokens[at].take(name, kind, part, false);
            }
            Op::Retract { kind, value } => {
                if let Some(fact) = reading
                    .elements()
                    .facts()
                    .iter()
                    .find(|fact| fact.kind == kind && fact.value == value)
                {
                    let alternative = Alternative {
                        span: fact.span,
                        text: fact.value.clone(),
                        taken: Sense {
                            kind: None,
                            rule: name,
                        },
                        passed: Sense {
                            kind: Some(fact.kind),
                            rule: fact.by,
                        },
                    };
                    reading.push_alternative(alternative);
                    reading.elements_mut().retract(kind, &value);
                }
            }
            Op::Instead { at, passed } => {
                let token = &tape.tokens[at];
                let taken = token
                    .taken
                    .iter()
                    .rev()
                    .find(|taking| taking.by == name)
                    .map(|taking| taking.kind);
                reading.push_alternative(Alternative {
                    span: token.span,
                    text: token.text.clone(),
                    taken: Sense {
                        kind: taken,
                        rule: name,
                    },
                    passed,
                });
            }
        }
    }
    // Only a later verdict's episode settles against this one's, so a
    // batch read off one prefix keeps both ends.
    if verdict.provisional {
        reading.set_provisional_episode();
    }
}

fn run_rule(rule: &Rule, tape: &mut Tape, reading: &mut Reading, options: &Options) {
    if !(rule.gate)(options) {
        return;
    }
    if rule
        .settles
        .is_some_and(|kind| reading.elements().contains(kind))
    {
        return;
    }
    let verdict = (rule.read)(tape, reading);
    apply(rule.name, rule.certainty, verdict, tape, reading);
}

fn run_words(rules: &[WordRule], until: ElementKind, tape: &mut Tape, reading: &mut Reading) {
    let mut at = 0;
    while at < tape.len() {
        for rule in rules {
            if !tape.tokens[at].is_free() {
                break;
            }
            let verdict = (rule.read)(tape, reading, at);
            if verdict.is_nothing() {
                continue;
            }
            let done = verdict.takes(until);
            apply(rule.name, rule.certainty, verdict, tape, reading);
            if done {
                return;
            }
        }
        at += 1;
    }
}

/// Runs every step of [`RULES`] in order, or up to and including the one
/// that carries `until`.
pub(crate) fn run(
    mut tape: Tape,
    elements: Elements,
    options: &Options,
    until: Option<RuleName>,
) -> Reading {
    let mut reading = Reading::new(elements, options);
    for step in RULES {
        match step {
            Step::Tape(rule) => run_rule(rule, &mut tape, &mut reading, options),
            Step::Words { gate, rules, until } => {
                if gate(options) {
                    run_words(rules, *until, &mut tape, &mut reading);
                }
            }
            Step::Legacy(pass) => pass(&mut tape, &mut reading, options),
        }
        if until.is_some_and(|until| step.names().contains(&until)) {
            break;
        }
    }
    reading
}

/// Runs the table up to `rule` on `input` and returns what that rule read,
/// so one rule can be asserted against a realistic tape.
#[cfg(test)]
pub(crate) fn probe(rule: RuleName, input: &str) -> Vec<(ElementKind, String)> {
    crate::parse_until(input, &Options::default(), Some(rule))
        .elements()
        .facts()
        .iter()
        .filter(|fact| fact.by == rule)
        .map(|fact| (fact.kind, fact.value.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::fact;
    use crate::token::{Shape, Token};

    #[test]
    fn labels_round_trip_for_every_rule() {
        for name in RuleName::ALL {
            assert_eq!(RuleName::from_label(name.label()), Some(name));
        }
        assert_eq!(RuleName::from_label("no_such_rule"), None);
    }

    #[test]
    fn every_rule_in_the_table_is_named_once() {
        let mut names: Vec<RuleName> = RULES.iter().flat_map(Step::names).collect();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count);
        for name in names {
            assert!(RuleName::ALL.contains(&name));
        }
    }

    fn word(text: &str, start: usize) -> Token {
        Token::new(
            Shape::Word,
            text.to_owned(),
            Span {
                start,
                end: start + text.len(),
            },
            false,
            None,
        )
    }

    fn tape() -> Tape {
        Tape::new(vec![
            word("Show", 0),
            Token::new(
                Shape::Delimiter,
                " ".to_owned(),
                Span { start: 4, end: 5 },
                false,
                None,
            ),
            word("05v2", 5),
        ])
    }

    fn reading() -> Reading {
        Reading::new(Elements::default(), &Options::default())
    }

    fn facts(reading: &Reading) -> Vec<(ElementKind, &str, Span)> {
        reading
            .elements()
            .facts()
            .iter()
            .map(|fact| (fact.kind, fact.value.as_str(), fact.span))
            .collect()
    }

    #[test]
    fn take_part_reads_two_values_off_one_token() {
        let mut tape = tape();
        let mut reading = reading();
        let verdict = Verdict::nothing()
            .take_part(ElementKind::EpisodeNumber, 2, 0..2, "05")
            .take_part(ElementKind::ReleaseVersion, 2, 2..4, "2");
        apply(
            RuleName::Legacy,
            Certainty::Stated,
            verdict,
            &mut tape,
            &mut reading,
        );
        assert_eq!(
            facts(&reading),
            [
                (ElementKind::EpisodeNumber, "05", Span { start: 5, end: 7 }),
                (ElementKind::ReleaseVersion, "2", Span { start: 7, end: 9 }),
            ]
        );
        assert!(!tape.tokens[2].is_free());
        assert!(tape.tokens[0].is_free());
        assert_eq!(reading.elements().facts()[0].certainty, Certainty::Stated);
    }

    #[test]
    fn take_run_folds_the_run_and_takes_its_free_tokens() {
        let mut tape = tape();
        let mut reading = reading();
        let verdict =
            Verdict::nothing().take_run(ElementKind::AnimeTitle, 0..3, Delimiters::Folded);
        apply(
            RuleName::Legacy,
            Certainty::Shaped,
            verdict,
            &mut tape,
            &mut reading,
        );
        assert_eq!(
            facts(&reading),
            [(
                ElementKind::AnimeTitle,
                "Show 05v2",
                Span { start: 0, end: 9 }
            )]
        );
        assert!(tape.tokens.iter().all(|token| !token.is_free()));
    }

    #[test]
    fn a_held_taking_records_the_value_and_leaves_the_word() {
        let mut tape = tape();
        let mut reading = reading();
        apply(
            RuleName::Legacy,
            Certainty::Stated,
            Verdict::nothing().hold(ElementKind::AnimeType, 0),
            &mut tape,
            &mut reading,
        );
        assert_eq!(reading.elements().get(ElementKind::AnimeType), Some("Show"));
        assert!(tape.tokens[0].is_free());
    }

    #[test]
    fn spend_takes_without_a_fact() {
        let mut tape = tape();
        let mut reading = reading();
        apply(
            RuleName::Legacy,
            Certainty::Stated,
            Verdict::nothing().spend(ElementKind::EpisodePrefix, 0),
            &mut tape,
            &mut reading,
        );
        assert!(reading.elements().is_empty());
        assert!(!tape.tokens[0].is_free());
    }

    #[test]
    fn a_second_fact_of_a_singular_kind_refuses_the_whole_verdict() {
        let mut tape = tape();
        let mut elements = Elements::default();
        elements.push(fact(ElementKind::ReleaseVersion, "1"));
        let mut reading = Reading::new(elements, &Options::default());
        let verdict = Verdict::nothing()
            .take_part(ElementKind::EpisodeNumber, 2, 0..2, "05")
            .take_part(ElementKind::ReleaseVersion, 2, 2..4, "2");
        apply(
            RuleName::Legacy,
            Certainty::Stated,
            verdict,
            &mut tape,
            &mut reading,
        );
        assert_eq!(reading.elements().len(), 1);
        assert!(tape.tokens[2].is_free());
    }

    #[test]
    fn a_provisional_episode_settles_two_schemes_by_size() {
        let mut tape = Tape::new(vec![
            word("08", 0),
            word("05", 3),
            word("05", 6),
            word("12", 9),
        ]);
        let mut reading = reading();
        let by = RuleName::Legacy;
        apply(
            by,
            Certainty::Stated,
            Verdict::nothing()
                .take(ElementKind::EpisodeNumber, 0)
                .provisional(),
            &mut tape,
            &mut reading,
        );
        apply(
            by,
            Certainty::Stated,
            Verdict::nothing().take(ElementKind::EpisodeNumber, 1),
            &mut tape,
            &mut reading,
        );
        assert_eq!(
            reading.elements().get_all(ElementKind::EpisodeNumberAlt),
            ["08"]
        );
        assert_eq!(
            reading.elements().get_all(ElementKind::EpisodeNumber),
            ["05"]
        );
        apply(
            by,
            Certainty::Stated,
            Verdict::nothing().take(ElementKind::EpisodeNumber, 2),
            &mut tape,
            &mut reading,
        );
        assert_eq!(reading.elements().len(), 2);
        assert!(!tape.tokens[2].is_free());
        apply(
            by,
            Certainty::Stated,
            Verdict::nothing().take(ElementKind::EpisodeNumber, 3),
            &mut tape,
            &mut reading,
        );
        assert_eq!(
            reading.elements().get_all(ElementKind::EpisodeNumberAlt),
            ["08", "12"]
        );
    }

    #[test]
    fn a_retraction_records_what_the_text_could_have_been() {
        let mut tape = tape();
        let mut elements = Elements::default();
        elements.push(Fact {
            span: Span { start: 0, end: 4 },
            ..fact(ElementKind::Other, "Show")
        });
        let mut reading = Reading::new(elements, &Options::default());
        apply(
            RuleName::Prelude,
            Certainty::Stated,
            Verdict::nothing().retract(ElementKind::Other, "Show"),
            &mut tape,
            &mut reading,
        );
        assert!(reading.elements().is_empty());
        let alternative = &reading.alternatives()[0];
        assert_eq!(alternative.text, "Show");
        assert_eq!(alternative.taken.kind, None);
        assert_eq!(alternative.passed.kind, Some(ElementKind::Other));
        assert_eq!(alternative.passed.rule, RuleName::Legacy);
    }

    #[test]
    fn instead_names_the_kind_the_rule_took_and_the_one_it_passed() {
        let mut tape = tape();
        let mut reading = reading();
        let passed = Sense {
            kind: None,
            rule: RuleName::Legacy,
        };
        apply(
            RuleName::Legacy,
            Certainty::Guessed,
            Verdict::nothing()
                .take(ElementKind::EpisodeNumber, 2)
                .instead(2, passed),
            &mut tape,
            &mut reading,
        );
        let alternative = &reading.alternatives()[0];
        assert_eq!(alternative.taken.kind, Some(ElementKind::EpisodeNumber));
        assert_eq!(alternative.passed, passed);
        assert_eq!(alternative.span, Span { start: 5, end: 9 });
    }
}
