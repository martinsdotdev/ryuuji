use super::Parser;
use crate::element::ElementKind;
use crate::token;

impl Parser<'_> {
    pub(super) fn search_release_group(&mut self) {
        if self.elements.contains(ElementKind::ReleaseGroup) {
            return;
        }
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
        self.record(ElementKind::ReleaseGroup, group, last);
    }
}
