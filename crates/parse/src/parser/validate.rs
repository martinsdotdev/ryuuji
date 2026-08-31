use super::Parser;
use crate::element::ElementKind;

impl Parser<'_> {
    pub(super) fn validate_elements(&mut self) {
        let Some(episode_title) = self.elements.get(ElementKind::EpisodeTitle) else {
            return;
        };
        let title_lower = episode_title.to_lowercase();
        let anime_types: Vec<String> = self
            .elements
            .get_all(ElementKind::AnimeType)
            .into_iter()
            .map(str::to_owned)
            .collect();
        for value in anime_types {
            let value_lower = value.to_lowercase();
            if title_lower == value_lower {
                self.elements.remove(ElementKind::EpisodeTitle);
            } else if title_lower.contains(&value_lower)
                && self
                    .table
                    .find(ElementKind::AnimeType, &value.to_uppercase())
                    .is_some()
            {
                self.elements.remove_value(ElementKind::AnimeType, &value);
            }
        }
    }
}
