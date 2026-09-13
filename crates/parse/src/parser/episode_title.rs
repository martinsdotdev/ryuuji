use super::Parser;
use crate::element::ElementKind;
use crate::string;

impl Parser<'_> {
    pub(super) fn search_episode_title(&mut self) {
        if !self.elements.contains(ElementKind::EpisodeNumber) {
            return;
        }
        let Some(run) = self
            .tape
            .free_runs(false)
            .find(|run| run.len() > 2 || !string::is_dash(&self.tape.tokens[run.start].text))
        else {
            return;
        };
        self.build_and_insert(ElementKind::EpisodeTitle, run.start, run.end, false);
    }
}
