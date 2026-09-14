use super::Parser;
use crate::element::ElementKind;

impl Parser<'_> {
    /// An unidentifiable keyword (`Ita`, `END`, `Opus`, ...) leaves its
    /// token in place so a title can keep it, and that is also how it tells
    /// the two apart: a keyword that turned out to be a word of the anime
    /// title was never an element, and an episode title that is nothing but
    /// such a keyword was never a title.
    ///
    /// Anime types are exempt from the anime-title check: the corpus keeps
    /// `Movie` wherever it sits (`The New Movie Q`, `Movie Part 1`) yet
    /// drops `Special` when a word follows it (`Special A`), and nothing but
    /// the word itself separates the two. They keep only anitomy's
    /// episode-title check.
    pub(super) fn validate_elements(&mut self) {
        let episode_title = self
            .elements
            .get(ElementKind::EpisodeTitle)
            .map(str::to_owned);
        let unidentifiable: Vec<(ElementKind, String)> = self
            .elements
            .iter()
            .filter(|(kind, value)| {
                self.table
                    .find(*kind, &value.to_uppercase())
                    .is_some_and(|keyword| !keyword.identifiable)
            })
            .map(|(kind, value)| (kind, value.to_owned()))
            .collect();
        for (kind, value) in unidentifiable {
            let Some(title) = episode_title.as_deref() else {
                continue;
            };
            if title.eq_ignore_ascii_case(&value) {
                self.elements.retract(ElementKind::EpisodeTitle, title);
            } else if has_word(title, &value) {
                self.elements.retract(kind, &value);
            }
        }
    }
}

/// Whether `word` appears whole in `text`, case-insensitively, between
/// non-alphanumeric characters or the ends.
fn has_word(text: &str, word: &str) -> bool {
    text.split(|c: char| !c.is_alphanumeric())
        .any(|candidate| candidate.eq_ignore_ascii_case(word))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_word_matches_whole_and_case_insensitively() {
        assert!(has_word("Bokura Ga Ita", "ITA"));
        assert!(has_word("The End of Evangelion", "end"));
        assert!(!has_word("Weekend", "END"));
        assert!(!has_word("Specials", "Special"));
    }
}
