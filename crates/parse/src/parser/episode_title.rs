use super::Parser;
use crate::element::ElementKind;
use crate::string;

impl Parser<'_> {
    pub(super) fn search_episode_title(&mut self) {
        if !self.elements.contains(ElementKind::EpisodeNumber) {
            return;
        }
        let Some((begin, end)) = self
            .unknown_spans(false)
            .find(|&(begin, end)| end - begin > 2 || !string::is_dash(&self.tokens[begin].content))
        else {
            return;
        };
        self.build_and_insert(ElementKind::EpisodeTitle, begin, end, false);
    }
}
