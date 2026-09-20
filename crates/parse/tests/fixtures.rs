use std::collections::BTreeMap;

use ryuuji_parse::{Certainty, ElementKind, Elements, Options, Reading, RuleName, parse};
use serde::Deserialize;

fn parsed_map(elements: &Elements) -> BTreeMap<String, Vec<String>> {
    let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (kind, value) in elements.iter() {
        if kind == ElementKind::FileName {
            continue;
        }
        map.entry(kind.label().to_owned())
            .or_default()
            .push(value.to_owned());
    }
    map
}

/// The expected elements of a case, in the same shape `parsed_map` returns.
fn expected_map(elements: &BTreeMap<String, OneOrMany>) -> BTreeMap<String, Vec<String>> {
    elements
        .iter()
        .map(|(label, values)| (label.clone(), values.to_vec()))
        .collect()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    case: Vec<Case>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    name: String,
    input: String,
    #[serde(default = "default_true")]
    strict: bool,
    #[serde(default)]
    options: Options,
    elements: BTreeMap<String, OneOrMany>,
    /// The rule each listed kind's first value must have come from.
    #[serde(default)]
    by: BTreeMap<String, String>,
    /// A doubt the reading must record.
    #[serde(default)]
    alternative: Vec<AlternativeCase>,
    /// Whether the name says it is something beside an episode. Read by the
    /// browser corpus, where it is half of what a case pins.
    #[serde(default)]
    extra: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AlternativeCase {
    text: String,
    /// A kind label, or `title_word` for text left where it stands.
    taken: String,
    passed: String,
    /// The rule that recorded the doubt.
    by: String,
}

fn sense(label: &str) -> Option<ElementKind> {
    if label == "title_word" {
        return None;
    }
    Some(ElementKind::from_label(label).unwrap_or_else(|| panic!("unknown sense {label:?}")))
}

fn rule(label: &str) -> RuleName {
    RuleName::from_label(label).unwrap_or_else(|| panic!("unknown rule {label:?}"))
}

/// Every way the reading fails what the case pins beyond the element map.
fn provenance_mismatches(case: &Case, reading: &Reading) -> Vec<String> {
    let mut mismatches = Vec::new();
    for (label, by) in &case.by {
        let kind =
            ElementKind::from_label(label).unwrap_or_else(|| panic!("unknown kind {label:?}"));
        let found = reading.elements().fact(kind).map(|fact| fact.by);
        if found != Some(rule(by)) {
            mismatches.push(format!(
                "{label} expected by {by:?}, read by {:?}",
                found.map(RuleName::label)
            ));
        }
    }
    for expected in &case.alternative {
        let (taken, passed, by) = (
            sense(&expected.taken),
            sense(&expected.passed),
            rule(&expected.by),
        );
        let found = reading.alternatives().iter().any(|alternative| {
            alternative.text == expected.text
                && alternative.taken.kind == taken
                && alternative.passed.kind == passed
                && alternative.taken.rule == by
        });
        if !found {
            mismatches.push(format!(
                "alternative {:?} taken {:?} passed {:?} by {:?} not recorded; recorded: {:?}",
                expected.text,
                expected.taken,
                expected.passed,
                expected.by,
                reading
                    .alternatives()
                    .iter()
                    .map(|alternative| (
                        alternative.text.as_str(),
                        alternative.taken.kind.map(ElementKind::label),
                        alternative.passed.kind.map(ElementKind::label),
                        alternative.taken.rule.label(),
                    ))
                    .collect::<Vec<_>>()
            ));
        }
    }
    mismatches
}

fn default_true() -> bool {
    true
}

/// A fixture value: one string, one bare number, or a list. anitomy's JSON
/// spells some counts unquoted, so the number arm is not optional.
#[derive(Deserialize)]
#[serde(untagged)]
enum OneOrMany {
    One(String),
    Number(i64),
    Many(Vec<String>),
}

impl OneOrMany {
    fn to_vec(&self) -> Vec<String> {
        match self {
            OneOrMany::One(value) => vec![value.clone()],
            OneOrMany::Number(value) => vec![value.to_string()],
            OneOrMany::Many(values) => values.clone(),
        }
    }
}

#[test]
fn ryuuji_fixtures_parse_exactly() {
    let document: Document =
        toml::from_str(include_str!("../fixtures/ryuuji.toml")).expect("ryuuji.toml parses");
    let mut mismatches = Vec::new();
    for case in &document.case {
        for label in case.elements.keys() {
            assert!(
                ElementKind::from_label(label).is_some(),
                "case {:?} expects unknown kind {label:?}",
                case.name
            );
        }
        let expected = expected_map(&case.elements);
        let reading = parse(&case.input, &case.options);
        let mut parsed = parsed_map(reading.elements());
        if !case.strict {
            parsed.retain(|label, _| expected.contains_key(label));
        }
        if parsed != expected {
            mismatches.push(format!(
                "case {:?}\n  input:    {:?}\n  expected: {expected:?}\n  parsed:   {parsed:?}",
                case.name, case.input
            ));
        }
        for mismatch in provenance_mismatches(case, &reading) {
            mismatches.push(format!("case {:?}\n  {mismatch}", case.name));
        }
    }
    assert!(mismatches.is_empty(), "\n{}", mismatches.join("\n"));
}

/// A rule the corpus never names is a rule nobody can change safely.
#[test]
fn every_rule_is_named_by_some_case() {
    let document: Document =
        toml::from_str(include_str!("../fixtures/ryuuji.toml")).expect("ryuuji.toml parses");
    let named: std::collections::BTreeSet<&str> = document
        .case
        .iter()
        .flat_map(|case| {
            case.by.values().map(String::as_str).chain(
                case.alternative
                    .iter()
                    .map(|alternative| alternative.by.as_str()),
            )
        })
        .collect();
    let unnamed: Vec<&str> = RuleName::ALL
        .iter()
        .map(|rule| rule.label())
        .filter(|label| !named.contains(label))
        .collect();
    assert!(unnamed.is_empty(), "rules no case names: {unnamed:?}");
}

/// A guessed episode is the only reading that carries a doubt, so every
/// case whose episode is a guess pins the alternative, and no other does.
#[test]
fn a_guessed_episode_always_records_an_alternative() {
    let document: Document =
        toml::from_str(include_str!("../fixtures/ryuuji.toml")).expect("ryuuji.toml parses");
    let browser: Document =
        toml::from_str(include_str!("../fixtures/browser.toml")).expect("browser.toml parses");
    for case in document.case.iter().chain(&browser.case) {
        let reading = parse(&case.input, &case.options);
        let guessed = reading
            .episodes()
            .is_some_and(|episodes| episodes.certainty == Certainty::Guessed);
        let doubted = reading
            .alternatives()
            .iter()
            .any(|alternative| alternative.taken.kind == Some(ElementKind::EpisodeNumber));
        assert_eq!(guessed, doubted, "case {:?}", case.name);
    }
}

/// One anitomy case. Its option keys carry an `option_` prefix, its `id` is
/// bookkeeping, and every remaining key is an expected element.
#[derive(Deserialize)]
struct AnitomyCase {
    file_name: String,
    /// Bookkeeping only, and one case spells it as a list, so take it as-is.
    #[allow(dead_code)]
    id: Option<serde_json::Value>,
    #[serde(rename = "option_allowed_delimiters")]
    allowed_delimiters: Option<String>,
    #[serde(rename = "option_ignored_strings")]
    ignored_strings: Option<Vec<String>>,
    #[serde(flatten)]
    expected: BTreeMap<String, OneOrMany>,
}

impl AnitomyCase {
    fn options(&self) -> Options {
        let mut options = Options::default();
        if let Some(value) = &self.allowed_delimiters {
            options.allowed_delimiters = value.clone();
        }
        if let Some(values) = &self.ignored_strings {
            options.ignored_strings = values.clone();
        }
        options
    }
}

fn run_anitomy() -> (usize, usize, Vec<String>) {
    let cases: Vec<AnitomyCase> = serde_json::from_str(include_str!("../fixtures/anitomy.json"))
        .expect("anitomy.json parses");
    let mut passed = 0;
    let mut failures = Vec::new();
    for (index, case) in cases.iter().enumerate() {
        let expected = expected_map(&case.expected);
        let parsed = parsed_map(parse(&case.file_name, &case.options()).elements());
        if parsed == expected {
            passed += 1;
        } else {
            failures.push(differences(index, &case.file_name, &expected, &parsed));
        }
    }
    (cases.len(), passed, failures)
}

/// The case's index and name, then one line per kind whose values differ,
/// so a report of every failure stays readable.
fn differences(
    index: usize,
    input: &str,
    expected: &BTreeMap<String, Vec<String>>,
    parsed: &BTreeMap<String, Vec<String>>,
) -> String {
    let mut lines = vec![format!("#{index} {input}")];
    let kinds: std::collections::BTreeSet<&String> = expected.keys().chain(parsed.keys()).collect();
    for kind in kinds {
        let (want, got) = (expected.get(kind), parsed.get(kind));
        if want != got {
            lines.push(format!("    {kind}: expected {want:?} parsed {got:?}"));
        }
    }
    lines.join("\n")
}

/// Every span a reading reports slices the caller's input on char
/// boundaries, whatever the options removed from it first.
fn span_mismatches(input: &str, reading: &Reading) -> Vec<String> {
    let slices = |start: usize, end: usize| {
        start <= end
            && end <= input.len()
            && input.is_char_boundary(start)
            && input.is_char_boundary(end)
    };
    let facts = reading
        .elements()
        .facts()
        .iter()
        .filter(|fact| !slices(fact.span.start, fact.span.end))
        .map(|fact| {
            format!(
                "{} {:?} span {:?}",
                fact.kind.label(),
                fact.value,
                fact.span
            )
        });
    let alternatives = reading
        .alternatives()
        .iter()
        .filter(|alternative| !slices(alternative.span.start, alternative.span.end))
        .map(|alternative| {
            format!(
                "alternative {:?} span {:?}",
                alternative.text, alternative.span
            )
        });
    facts
        .chain(alternatives)
        .map(|mismatch| format!("{input:?}: {mismatch}"))
        .collect()
}

#[test]
fn every_span_slices_its_input() {
    let document: Document =
        toml::from_str(include_str!("../fixtures/ryuuji.toml")).expect("ryuuji.toml parses");
    let anitomy: Vec<AnitomyCase> = serde_json::from_str(include_str!("../fixtures/anitomy.json"))
        .expect("anitomy.json parses");
    let browser: Document =
        toml::from_str(include_str!("../fixtures/browser.toml")).expect("browser.toml parses");
    let ryuuji = document
        .case
        .iter()
        .map(|case| (case.input.as_str(), case.options.clone()));
    let anitomy = anitomy
        .iter()
        .map(|case| (case.file_name.as_str(), case.options()));
    let browser = browser
        .case
        .iter()
        .map(|case| (case.input.as_str(), case.options.clone()));
    let mismatches: Vec<String> = ryuuji
        .chain(anitomy)
        .chain(browser)
        .flat_map(|(input, options)| span_mismatches(input, &parse(input, &options)))
        .collect();
    assert!(mismatches.is_empty(), "\n{}", mismatches.join("\n"));
}

/// The browser corpus, compared the way a loose ryuuji case is, plus what
/// each name says it is. A case pins what Ryuuji should read, so the shapes
/// a parser that sees only a title cannot reach stay failing here.
fn run_browser() -> (usize, usize, Vec<String>) {
    let document: Document =
        toml::from_str(include_str!("../fixtures/browser.toml")).expect("browser.toml parses");
    let mut passed = 0;
    let mut failures = Vec::new();
    for (index, case) in document.case.iter().enumerate() {
        let expected = expected_map(&case.elements);
        let reading = parse(&case.input, &case.options);
        let mut parsed = parsed_map(reading.elements());
        if !case.strict {
            parsed.retain(|label, _| expected.contains_key(label));
        }
        let mut failure = if parsed == expected {
            String::new()
        } else {
            differences(index, &case.input, &expected, &parsed)
        };
        let extra = reading.extra().is_some();
        if extra != case.extra {
            if failure.is_empty() {
                failure = format!("#{index} {}", case.input);
            }
            failure.push_str(&format!(
                "\n    extra: expected {} parsed {extra}",
                case.extra
            ));
        }
        if failure.is_empty() {
            passed += 1;
        } else {
            failures.push(failure);
        }
    }
    (document.case.len(), passed, failures)
}

const BROWSER_BASELINE: usize = 125;

/// Every failing case: `cargo test -p ryuuji-parse -- --ignored
/// browser_report --nocapture`.
#[test]
#[ignore]
fn browser_report() {
    let (total, passed, failures) = run_browser();
    eprintln!("browser conformance: {passed}/{total} passed");
    for failure in &failures {
        eprintln!("{failure}");
    }
}

#[test]
fn browser_pass_count_never_regresses() {
    let (total, passed, _) = run_browser();
    assert_eq!(total, 164);
    assert!(
        passed >= BROWSER_BASELINE,
        "browser passes regressed: {passed} < {BROWSER_BASELINE}"
    );
}

const ANITOMY_BASELINE: usize = 156;

/// Every failing case, not a sample: `cargo test -p ryuuji-parse -- --ignored
/// anitomy_report --nocapture`.
#[test]
#[ignore]
fn anitomy_report() {
    let (total, passed, failures) = run_anitomy();
    eprintln!("anitomy conformance: {passed}/{total} passed");
    for failure in &failures {
        eprintln!("{failure}");
    }
}

#[test]
fn anitomy_pass_count_never_regresses() {
    let (total, passed, _) = run_anitomy();
    assert_eq!(total, 182);
    assert!(
        passed >= ANITOMY_BASELINE,
        "anitomy passes regressed: {passed} < {ANITOMY_BASELINE}"
    );
}
