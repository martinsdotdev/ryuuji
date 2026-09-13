//! The rule engine. For now only the names rules go by; the table and the
//! runner arrive as the rules are lifted out of the legacy parser.

/// A rule's name: its fixture key, its Diagnostics label, and what a fact
/// reports as its reader. Closed, so a fixture cannot name a rule that does
/// not exist.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RuleName {
    /// The extension strip and the file name, read before any rule runs.
    Prelude,
    /// The passes not yet lifted into rules of their own.
    Legacy,
}

impl RuleName {
    pub const ALL: [RuleName; 2] = [RuleName::Prelude, RuleName::Legacy];

    pub fn label(self) -> &'static str {
        match self {
            RuleName::Prelude => "prelude",
            RuleName::Legacy => "legacy",
        }
    }

    pub fn from_label(label: &str) -> Option<RuleName> {
        RuleName::ALL.into_iter().find(|name| name.label() == label)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_round_trip_for_every_rule() {
        for name in RuleName::ALL {
            assert_eq!(RuleName::from_label(name.label()), Some(name));
        }
        assert_eq!(RuleName::from_label("no_such_rule"), None);
    }
}
