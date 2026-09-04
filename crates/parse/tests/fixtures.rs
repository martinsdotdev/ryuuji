use std::collections::BTreeMap;

use ryuuji_parse::{ElementKind, Elements, Options, parse};
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
        let mut parsed = parsed_map(&parse(&case.input, &case.options));
        if !case.strict {
            parsed.retain(|label, _| expected.contains_key(label));
        }
        if parsed != expected {
            mismatches.push(format!(
                "case {:?}\n  input:    {:?}\n  expected: {expected:?}\n  parsed:   {parsed:?}",
                case.name, case.input
            ));
        }
    }
    assert!(mismatches.is_empty(), "\n{}", mismatches.join("\n"));
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
    for case in &cases {
        let expected = expected_map(&case.expected);
        let parsed = parsed_map(&parse(&case.file_name, &case.options()));
        if parsed == expected {
            passed += 1;
        } else {
            let input = &case.file_name;
            failures.push(format!(
                "{input}
  expected: {expected:?}
  parsed:   {parsed:?}"
            ));
        }
    }
    (cases.len(), passed, failures)
}

const ANITOMY_BASELINE: usize = 134;

#[test]
#[ignore]
fn anitomy_report() {
    let (total, passed, failures) = run_anitomy();
    eprintln!("anitomy conformance: {passed}/{total} passed");
    for failure in failures.iter().take(20) {
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
