//! The wire between the library and a person's list on a service. The core
//! says what it wants done ([`Work`](ryuuji_core::Work)) and takes what came
//! of it ([`Report`](ryuuji_core::Report)); this crate owns everything in
//! between: one worker thread, a lane per service with its pauses and
//! backoffs, the requests and answers of each service, and the token, which
//! never leaves the worker and its vault.

mod anilist;
mod engine;
mod http;
mod reconcile;
mod service;
mod vault;
mod worker;

use std::fmt;
use std::time::SystemTime;

pub use engine::Pause;
pub use worker::{LastCall, Paused, ServiceFacts, SyncError, Syncer, start};

/// An access token as pasted from the service: a JWT whose payload says when
/// it stops working. It prints as `Token(<redacted>)`, has no `Display` and
/// is never serialized, so a log line or a debug dump of anything holding
/// one shows no secret.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(<redacted>)")
    }
}

impl Token {
    /// Trims the paste and checks it has a JWT's shape, three dot-separated
    /// base64url segments with a payload that reads as JSON, before it is
    /// sent anywhere: a mispaste fails in the Settings card rather than as
    /// a refusal from the service.
    pub fn parse(pasted: &str) -> Result<Token, TokenError> {
        let trimmed = pasted.trim();
        if trimmed.is_empty() {
            return Err(TokenError::Empty);
        }
        let segments: Vec<&str> = trimmed.split('.').collect();
        let base64url = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_';
        if segments.len() != 3
            || segments
                .iter()
                .any(|segment| segment.is_empty() || !segment.bytes().all(base64url))
            || anilist::jwt_payload(trimmed).is_none()
        {
            return Err(TokenError::NotAToken);
        }
        Ok(Token(trimmed.to_owned()))
    }

    /// When the token stops working, read unsigned and unverified from its
    /// payload: enough for the Settings card's date and for refusing to
    /// send a token that has passed, and used for nothing else.
    pub(crate) fn expires(&self) -> Option<SystemTime> {
        anilist::expiry(&self.0)
    }

    /// Whether `exp` has passed as of `now`; a token that names no `exp`
    /// has not.
    pub(crate) fn expired_at(&self, now: SystemTime) -> bool {
        self.expires().is_some_and(|at| at <= now)
    }

    /// The text that goes in the `Authorization` header, and nowhere else.
    pub(crate) fn secret(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TokenError {
    #[error("nothing was pasted")]
    Empty,
    #[error("this is not a token: one has three dot-separated parts")]
    NotAToken,
}

/// How a person signs in to a service. MyAnimeList and Kitsu add their own
/// variants.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Credentials {
    Token(Token),
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use base64::Engine;
    use base64::prelude::BASE64_URL_SAFE_NO_PAD;
    use ryuuji_core::{ListEntryId, ListState, ListStatus, MediaId, Patch, Write};

    use super::*;
    use crate::anilist;

    /// A JWT with the given payload and a fixed header and signature; only
    /// its shape and payload matter here, since nothing verifies it.
    pub(crate) fn jwt(payload: &str) -> String {
        let header = BASE64_URL_SAFE_NO_PAD.encode(r#"{"alg":"RS256","typ":"JWT"}"#);
        let payload = BASE64_URL_SAFE_NO_PAD.encode(payload);
        let signature = BASE64_URL_SAFE_NO_PAD.encode(b"not a real signature");
        format!("{header}.{payload}.{signature}")
    }

    /// A token good until `exp` seconds after the epoch.
    pub(crate) fn token_until(exp: u64) -> Token {
        Token::parse(&jwt(&format!(
            r#"{{"aud":"12345","jti":"abc","iat":1758585600,"exp":{exp},"sub":"7","scopes":[]}}"#
        )))
        .unwrap()
    }

    #[test]
    fn a_paste_is_trimmed_and_checked_for_a_jwts_shape() {
        let text = jwt(r#"{"sub":"1"}"#);
        let token = Token::parse(&format!("  {text}\r\n")).unwrap();
        assert_eq!(token.secret(), text);

        assert_eq!(Token::parse("  \n"), Err(TokenError::Empty));
        assert_eq!(Token::parse("abc"), Err(TokenError::NotAToken));
        assert_eq!(Token::parse("a.b"), Err(TokenError::NotAToken));
        assert_eq!(Token::parse("a..c"), Err(TokenError::NotAToken));
        assert_eq!(Token::parse("a.b.c.d"), Err(TokenError::NotAToken));
        // A padding character is not base64url as a JWT writes it.
        assert_eq!(Token::parse("aa.bb=.cc"), Err(TokenError::NotAToken));
        assert_eq!(Token::parse("aa.b b.cc"), Err(TokenError::NotAToken));
        // The right alphabet, but the payload is not JSON.
        assert_eq!(Token::parse("aaaa.bbbb.cccc"), Err(TokenError::NotAToken));
        let array = jwt("[1, 2]");
        assert_eq!(Token::parse(&array), Err(TokenError::NotAToken));
    }

    #[test]
    fn the_expiry_is_read_from_the_payload_unverified() {
        let token = token_until(1_790_121_600);
        assert_eq!(
            token.expires(),
            Some(UNIX_EPOCH + Duration::from_secs(1_790_121_600))
        );
        assert!(!token.expired_at(UNIX_EPOCH + Duration::from_secs(1_790_121_599)));
        assert!(token.expired_at(UNIX_EPOCH + Duration::from_secs(1_790_121_600)));
        let no_exp = Token::parse(&jwt(r#"{"sub":"1"}"#)).unwrap();
        assert_eq!(no_exp.expires(), None);
        assert!(!no_exp.expired_at(SystemTime::now()));
    }

    #[test]
    fn nothing_that_holds_a_token_prints_it() {
        let token = token_until(1_790_121_600);
        let secret = token.secret().to_owned();
        assert_eq!(format!("{token:?}"), "Token(<redacted>)");
        let credentials = Credentials::Token(token);
        assert_eq!(format!("{credentials:?}"), "Token(Token(<redacted>))");
        let state = ListState {
            status: ListStatus::Watching,
            progress: 4,
            repeat: 0,
        };
        let requests = [
            anilist::viewer(),
            anilist::find("Sousou no Frieren"),
            anilist::read(MediaId(154587)),
            anilist::save(MediaId(154587), None, Write::Create(state)),
            anilist::save(
                MediaId(154587),
                Some(ListEntryId(381_234_567)),
                Write::Change(Patch {
                    progress: Some(4),
                    ..Patch::default()
                }),
            ),
        ];
        for request in requests {
            let shown = format!("{request:?}");
            assert!(request.signed);
            assert!(!shown.contains(&secret), "{shown}");
            for segment in secret.split('.') {
                assert!(!shown.contains(segment), "{shown}");
            }
        }
    }
}
