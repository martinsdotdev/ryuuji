//! Tokens kept in memory for the life of the process: the store where the
//! platform has none of its own, and the one every worker test runs over.
//! A relaunch starts signed out. Nothing here fails, except where a test
//! asks it to.

use std::collections::HashMap;

use ryuuji_core::Service;

use crate::Token;
use crate::vault::VaultError;

#[derive(Default)]
pub(crate) struct Tokens {
    kept: HashMap<Service, Token>,
    /// What `keep` answers instead of keeping; see [`Tokens::refusing`].
    #[cfg(test)]
    refuse_keep: Option<Box<dyn Fn() -> VaultError + Send>>,
}

impl Tokens {
    /// A store that refuses every `keep` with what `refuse` answers, so a
    /// worker test can watch a sign-in meet one.
    #[cfg(test)]
    pub(crate) fn refusing(refuse: impl Fn() -> VaultError + Send + 'static) -> Tokens {
        Tokens {
            kept: HashMap::new(),
            refuse_keep: Some(Box::new(refuse)),
        }
    }

    pub(crate) fn keep(&mut self, service: Service, token: Token) -> Result<(), VaultError> {
        #[cfg(test)]
        if let Some(refuse) = &self.refuse_keep {
            return Err(refuse());
        }
        self.kept.insert(service, token);
        Ok(())
    }

    pub(crate) fn forget(&mut self, service: Service) -> Result<(), VaultError> {
        self.kept.remove(&service);
        Ok(())
    }

    pub(crate) fn token(&self, service: Service) -> Result<Option<Token>, VaultError> {
        Ok(self.kept.get(&service).cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::token_until;

    #[test]
    fn a_kept_token_is_read_back_until_forgotten() {
        let mut tokens = Tokens::default();
        assert_eq!(tokens.token(Service::AniList), Ok(None));
        let token = token_until(1_790_121_600);
        tokens.keep(Service::AniList, token.clone()).unwrap();
        assert_eq!(tokens.token(Service::AniList), Ok(Some(token.clone())));
        let newer = token_until(1_800_000_000);
        tokens.keep(Service::AniList, newer.clone()).unwrap();
        assert_eq!(tokens.token(Service::AniList), Ok(Some(newer)));
        tokens.forget(Service::AniList).unwrap();
        assert_eq!(tokens.token(Service::AniList), Ok(None));
        tokens.forget(Service::AniList).unwrap();
    }

    #[test]
    fn a_refusal_a_test_asks_for_keeps_nothing() {
        let mut tokens = Tokens::refusing(|| VaultError::NotAToken);
        assert_eq!(
            tokens.keep(Service::AniList, token_until(1_790_121_600)),
            Err(VaultError::NotAToken)
        );
        assert_eq!(tokens.token(Service::AniList), Ok(None));
    }
}
