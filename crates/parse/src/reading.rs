//! What one name says, and how sure the parser is of it.

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
