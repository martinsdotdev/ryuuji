use super::Parser;
use crate::element::ElementKind;
use crate::token;

impl Parser<'_> {
    pub(super) fn search_release_group(&mut self) {
        if self.elements.contains(ElementKind::ReleaseGroup) {
            return;
        }
        let Some(run) = self.tape.free_runs(true).find(|run| {
            run.end < self.tape.len()
                && self.tape.tokens[run.end].is_bracket()
                && self
                    .tape
                    .prev(run.start, token::is_not_delimiter)
                    .is_none_or(|prev| self.tape.tokens[prev].is_bracket())
        }) else {
            self.search_trailing_release_group();
            return;
        };
        self.build_and_insert(ElementKind::ReleaseGroup, run.start, run.end, true);
    }

    /// Scene-style naming hangs the group off the end after a dash
    /// (`...(BDrip 1920x1080 x264)-ank.mkv`); the dash glues onto the word,
    /// so the last token outside the brackets reads `-ank`.
    fn search_trailing_release_group(&mut self) {
        let Some(last) = self.tape.prev(self.tape.len(), token::is_not_delimiter) else {
            return;
        };
        let token = &self.tape.tokens[last];
        if token.enclosed || !token.is_free() {
            return;
        }
        let Some(group) = token.text.strip_prefix('-') else {
            return;
        };
        if group.is_empty() || group.contains('-') {
            return;
        }
        let after_bracket = self
            .tape
            .prev(last, token::is_not_delimiter)
            .is_some_and(|prev| self.tape.tokens[prev].is_bracket());
        if !after_bracket {
            return;
        }
        let group = group.to_owned();
        self.retire(last, ElementKind::ReleaseGroup);
        self.elements.insert(ElementKind::ReleaseGroup, group);
    }
}
