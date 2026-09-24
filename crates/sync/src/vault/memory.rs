//! Tokens kept in memory for the life of the process: what every platform
//! has until it has a store of its own, and what tests run against. A
//! relaunch starts signed out.

use std::collections::HashMap;

use ryuuji_core::Service;

use crate::Token;

#[derive(Default)]
pub(crate) struct Tokens {
    kept: HashMap<Service, Token>,
}

impl Tokens {
    pub(crate) fn keep(&mut self, service: Service, token: Token) {
        self.kept.insert(service, token);
    }

    pub(crate) fn forget(&mut self, service: Service) {
        self.kept.remove(&service);
    }

    pub(crate) fn token(&self, service: Service) -> Option<Token> {
        self.kept.get(&service).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::token_until;

    #[test]
    fn a_kept_token_is_read_back_until_forgotten() {
        let mut tokens = Tokens::default();
        assert_eq!(tokens.token(Service::AniList), None);
        let token = token_until(1_790_121_600);
        tokens.keep(Service::AniList, token.clone());
        assert_eq!(tokens.token(Service::AniList), Some(token.clone()));
        let newer = token_until(1_800_000_000);
        tokens.keep(Service::AniList, newer.clone());
        assert_eq!(tokens.token(Service::AniList), Some(newer));
        tokens.forget(Service::AniList);
        assert_eq!(tokens.token(Service::AniList), None);
        tokens.forget(Service::AniList);
    }
}
