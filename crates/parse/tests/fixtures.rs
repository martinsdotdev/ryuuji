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
    options: CaseOptions,
    elements: BTreeMap<String, OneOrMany>,
}

fn default_true() -> bool {
    true
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct CaseOptions {
    allowed_delimiters: Option<String>,
    ignored_strings: Option<Vec<String>>,
    parse_episode_number: Option<bool>,
    parse_episode_title: Option<bool>,
    parse_file_extension: Option<bool>,
    parse_release_group: Option<bool>,
}

impl CaseOptions {
    fn build(&self) -> Options {
        let mut options = Options::default();
        if let Some(value) = &self.allowed_delimiters {
            options.allowed_delimiters = value.clone();
        }
        if let Some(value) = &self.ignored_strings {
            options.ignored_strings = value.clone();
        }
        if let Some(value) = self.parse_episode_number {
            options.parse_episode_number = value;
        }
        if let Some(value) = self.parse_episode_title {
            options.parse_episode_title = value;
        }
        if let Some(value) = self.parse_file_extension {
            options.parse_file_extension = value;
        }
        if let Some(value) = self.parse_release_group {
            options.parse_release_group = value;
        }
        options
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

impl OneOrMany {
    fn to_vec(&self) -> Vec<String> {
        match self {
            OneOrMany::One(value) => vec![value.clone()],
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
        let mut expected: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (label, values) in &case.elements {
            assert!(
                ElementKind::from_label(label).is_some(),
                "case {:?} expects unknown kind {label:?}",
                case.name
            );
            expected.insert(label.clone(), values.to_vec());
        }
        let elements = parse(&case.input, &case.options.build());
        let mut parsed = parsed_map(&elements);
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

fn run_anitomy() -> (usize, usize, Vec<String>) {
    let data: serde_json::Value = serde_json::from_str(include_str!("../fixtures/anitomy.json"))
        .expect("anitomy.json parses");
    let cases = data.as_array().expect("anitomy.json is an array");
    let mut passed = 0;
    let mut failures = Vec::new();
    for case in cases {
        let object = case.as_object().expect("each case is an object");
        let input = object["file_name"].as_str().expect("file_name is a string");
        let mut options = Options::default();
        if let Some(value) = object
            .get("option_allowed_delimiters")
            .and_then(|value| value.as_str())
        {
            options.allowed_delimiters = value.to_owned();
        }
        if let Some(values) = object
            .get("option_ignored_strings")
            .and_then(|value| value.as_array())
        {
            options.ignored_strings = values
                .iter()
                .map(|value| value.as_str().expect("ignored string").to_owned())
                .collect();
        }
        let mut expected: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (key, value) in object {
            if key == "file_name" || key == "id" || key.starts_with("option_") {
                continue;
            }
            let values = match value {
                serde_json::Value::String(text) => vec![text.clone()],
                serde_json::Value::Number(number) => vec![number.to_string()],
                serde_json::Value::Array(items) => items
                    .iter()
                    .map(|item| item.as_str().expect("array of strings").to_owned())
                    .collect(),
                other => panic!("unsupported expected value for {key}: {other:?}"),
            };
            expected.insert(key.clone(), values);
        }
        let parsed = parsed_map(&parse(input, &options));
        if parsed == expected {
            passed += 1;
        } else {
            failures.push(format!(
                "{input}\n  expected: {expected:?}\n  parsed:   {parsed:?}"
            ));
        }
    }
    (cases.len(), passed, failures)
}

const ANITOMY_BASELINE: usize = 2;

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
