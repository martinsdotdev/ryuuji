//! The keyword table: fixed terms the parser recognises and the flags that
//! control how each behaves. The table ships embedded in the binary.

use std::collections::HashMap;
use std::sync::LazyLock;

use serde::Deserialize;

use crate::element::ElementKind;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Keyword {
    pub(crate) kind: ElementKind,
    /// An identifiable keyword marks its token as an identifier, keeping it
    /// out of the title and episode passes.
    pub(crate) identifiable: bool,
    /// A searchable keyword may be matched by the keyword pass.
    pub(crate) searchable: bool,
    /// A valid keyword may stand as an element value on its own.
    pub(crate) valid: bool,
}

#[derive(Debug)]
pub(crate) struct KeywordTable {
    by_text: HashMap<String, Vec<Keyword>>,
    preidentified: Vec<(String, ElementKind)>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum TableError {
    #[error("keywords.toml does not parse")]
    Parse(#[source] toml::de::Error),
    #[error("unknown element kind {kind:?}")]
    UnknownKind { kind: String },
    #[error("unknown flags {flags:?}")]
    UnknownFlags { flags: String },
    #[error("empty value under kind {kind:?}")]
    EmptyValue { kind: String },
    #[error("keyword {value:?} is not upper-case")]
    NotUppercase { value: String },
    #[error("keyword {value:?} appears twice under kind {kind:?}")]
    Duplicate { kind: String, value: String },
}

#[derive(Deserialize)]
struct Document {
    #[serde(default)]
    keyword: Vec<KeywordGroup>,
    #[serde(default)]
    preidentified: Vec<PreidentifiedEntry>,
}

#[derive(Deserialize)]
struct KeywordGroup {
    kind: String,
    flags: String,
    values: Vec<String>,
}

#[derive(Deserialize)]
struct PreidentifiedEntry {
    text: String,
    kind: String,
}

fn parse_kind(label: &str) -> Result<ElementKind, TableError> {
    ElementKind::from_label(label).ok_or_else(|| TableError::UnknownKind {
        kind: label.to_owned(),
    })
}

impl KeywordTable {
    /// The embedded table; a parse failure is a build defect covered by a test.
    pub(crate) fn builtin() -> &'static KeywordTable {
        static TABLE: LazyLock<KeywordTable> = LazyLock::new(|| {
            KeywordTable::parse(include_str!("keywords.toml"))
                .expect("embedded keywords.toml is valid")
        });
        &TABLE
    }

    pub(crate) fn parse(text: &str) -> Result<KeywordTable, TableError> {
        let document: Document = toml::from_str(text).map_err(TableError::Parse)?;
        let mut by_text: HashMap<String, Vec<Keyword>> = HashMap::new();
        for group in document.keyword {
            let kind = parse_kind(&group.kind)?;
            let (identifiable, searchable, valid) = match group.flags.as_str() {
                "default" => (true, true, true),
                "invalid" => (true, true, false),
                "unidentifiable" => (false, true, true),
                "unidentifiable_invalid" => (false, true, false),
                "unidentifiable_unsearchable" => (false, false, true),
                _ => return Err(TableError::UnknownFlags { flags: group.flags }),
            };
            for value in group.values {
                if value.is_empty() {
                    return Err(TableError::EmptyValue { kind: group.kind });
                }
                if value.to_uppercase() != value {
                    return Err(TableError::NotUppercase { value });
                }
                let entries = by_text.entry(value.clone()).or_default();
                if entries.iter().any(|keyword| keyword.kind == kind) {
                    return Err(TableError::Duplicate {
                        kind: group.kind,
                        value,
                    });
                }
                entries.push(Keyword {
                    kind,
                    identifiable,
                    searchable,
                    valid,
                });
            }
        }
        let mut preidentified = Vec::new();
        for entry in document.preidentified {
            let kind = parse_kind(&entry.kind)?;
            if entry.text.is_empty() {
                return Err(TableError::EmptyValue { kind: entry.kind });
            }
            preidentified.push((entry.text, kind));
        }
        Ok(KeywordTable {
            by_text,
            preidentified,
        })
    }

    pub(crate) fn find(&self, kind: ElementKind, upper: &str) -> Option<Keyword> {
        self.by_text
            .get(upper)?
            .iter()
            .copied()
            .find(|keyword| keyword.kind == kind)
    }

    /// The first keyword for this text in table order; a value listed under
    /// two kinds resolves to whichever group appears first in keywords.toml.
    pub(crate) fn find_any(&self, upper: &str) -> Option<Keyword> {
        self.by_text.get(upper)?.first().copied()
    }

    pub(crate) fn preidentified(&self) -> &[(String, ElementKind)] {
        &self.preidentified
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count(table: &KeywordTable, kind: ElementKind) -> usize {
        table
            .by_text
            .values()
            .flatten()
            .filter(|keyword| keyword.kind == kind)
            .count()
    }

    #[test]
    fn builtin_table_parses_with_expected_counts() {
        let table = KeywordTable::builtin();
        assert_eq!(count(table, ElementKind::AnimeSeasonPrefix), 2);
        assert_eq!(count(table, ElementKind::EpisodePrefix), 13);
        assert_eq!(count(table, ElementKind::ReleaseVersion), 5);
        assert_eq!(table.preidentified().len(), 10);
    }

    #[test]
    fn duplicated_text_keeps_both_kinds_in_table_order() {
        let table = KeywordTable::builtin();
        assert_eq!(
            table.find_any("TS").map(|keyword| keyword.kind),
            Some(ElementKind::FileExtension)
        );
        assert_eq!(
            table
                .find(ElementKind::Other, "TS")
                .map(|keyword| keyword.kind),
            Some(ElementKind::Other)
        );
        assert_eq!(
            table.find_any("FLAC").map(|keyword| keyword.kind),
            Some(ElementKind::AudioTerm)
        );
    }

    #[test]
    fn bad_toml_is_a_parse_error() {
        let error = KeywordTable::parse("[[keyword]\nkind = 1").unwrap_err();
        assert!(matches!(error, TableError::Parse(_)));
    }

    #[test]
    fn unknown_kind_is_rejected() {
        let text = "[[keyword]]\nkind = \"nope\"\nflags = \"default\"\nvalues = [\"X\"]\n";
        let error = KeywordTable::parse(text).unwrap_err();
        assert!(matches!(error, TableError::UnknownKind { kind } if kind == "nope"));
    }

    #[test]
    fn unknown_flags_are_rejected() {
        let text = "[[keyword]]\nkind = \"source\"\nflags = \"bogus\"\nvalues = [\"X\"]\n";
        let error = KeywordTable::parse(text).unwrap_err();
        assert!(matches!(error, TableError::UnknownFlags { flags } if flags == "bogus"));
    }

    #[test]
    fn empty_value_is_rejected() {
        let text = "[[keyword]]\nkind = \"source\"\nflags = \"default\"\nvalues = [\"\"]\n";
        let error = KeywordTable::parse(text).unwrap_err();
        assert!(matches!(error, TableError::EmptyValue { kind } if kind == "source"));
    }

    #[test]
    fn lowercase_value_is_rejected() {
        let text = "[[keyword]]\nkind = \"source\"\nflags = \"default\"\nvalues = [\"Bd\"]\n";
        let error = KeywordTable::parse(text).unwrap_err();
        assert!(matches!(error, TableError::NotUppercase { value } if value == "Bd"));
    }

    #[test]
    fn duplicate_value_under_one_kind_is_rejected() {
        let text =
            "[[keyword]]\nkind = \"source\"\nflags = \"default\"\nvalues = [\"BD\", \"BD\"]\n";
        let error = KeywordTable::parse(text).unwrap_err();
        assert!(
            matches!(error, TableError::Duplicate { kind, value } if kind == "source" && value == "BD")
        );
    }
}
