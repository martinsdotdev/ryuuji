//! Where a token is kept between calls: Windows' Credential Manager, and
//! memory on every other target. The store is a module chosen for the
//! target, never a trait with one implementation; each module has the same
//! three items, and one that does not fails that target's build. Under test
//! the worker runs over memory on Windows too, so no test writes a real
//! credential; the Windows module is exercised by its own ignored round
//! trip under a test target name.

#[cfg(windows)]
#[cfg_attr(test, allow(dead_code))]
mod credential_manager;
#[cfg_attr(all(windows, not(test)), allow(dead_code))]
mod memory;

use std::fmt;

use ryuuji_core::Service;

use crate::Token;

#[cfg(all(windows, not(test)))]
pub(crate) use credential_manager::Tokens;
#[cfg(any(not(windows), test))]
pub(crate) use memory::Tokens;

/// The most bytes one credential's blob holds, Credential Manager's
/// `CRED_MAX_CREDENTIAL_BLOB_SIZE`. Written out so the check runs on every
/// target; the Windows module's tests hold it to the store's own limit.
pub(crate) const BLOB_CAP: usize = 2560;

/// Why the store could not do what was asked: the call and what the store
/// said, never the token.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum VaultError {
    /// The store failed or refused. `message` is its own account of why,
    /// which on Windows names the Win32 error code, so a Settings card says
    /// more than that something went wrong.
    #[error("{doing} failed: {message}")]
    Store { doing: Doing, message: String },
    #[error("the token is {bytes} bytes and Credential Manager keeps at most {cap}", cap = BLOB_CAP)]
    TooLong { bytes: usize },
    /// What was read back is not a token: something else wrote under
    /// Ryuuji's name, or a person edited it in the Credential Manager panel.
    #[error("the kept credential is not a token")]
    NotAToken,
}

/// The Credential Manager call that failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Doing {
    Write,
    Read,
    Delete,
}

impl fmt::Display for Doing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Doing::Write => "writing the credential",
            Doing::Read => "reading the credential",
            Doing::Delete => "deleting the credential",
        })
    }
}

/// `Ryuuji/<service tag>`: one credential per service, named so the
/// Credential Manager panel lists Ryuuji's together.
pub(crate) fn target_name(service: Service) -> String {
    format!("Ryuuji/{}", service.tag())
}

/// The blob a token is kept as: its UTF-8 bytes, refused past the cap
/// rather than cut, since a cut token would be kept and then refused by the
/// service.
pub(crate) fn blob(token: &Token) -> Result<&[u8], VaultError> {
    let bytes = token.secret().as_bytes();
    if bytes.len() > BLOB_CAP {
        return Err(VaultError::TooLong { bytes: bytes.len() });
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{jwt, token_until};

    #[test]
    fn the_target_name_is_ryuuji_slash_the_service_tag() {
        assert_eq!(target_name(Service::AniList), "Ryuuji/anilist");
    }

    #[test]
    fn the_blob_is_the_tokens_utf8_bytes_and_one_past_the_cap_is_refused() {
        let token = token_until(1_790_121_600);
        assert!(token.secret().len() <= BLOB_CAP);
        assert_eq!(blob(&token), Ok(token.secret().as_bytes()));

        let filler = "a".repeat(BLOB_CAP - token.secret().len());
        let at_cap = Token::parse(&format!("{}{filler}", token.secret())).unwrap();
        assert_eq!(at_cap.secret().len(), BLOB_CAP);
        assert_eq!(blob(&at_cap), Ok(at_cap.secret().as_bytes()));
        let past_cap = Token::parse(&format!("{}a", at_cap.secret())).unwrap();
        assert_eq!(
            blob(&past_cap),
            Err(VaultError::TooLong {
                bytes: BLOB_CAP + 1
            })
        );

        let pad = "a".repeat(BLOB_CAP);
        let long = Token::parse(&jwt(&format!(r#"{{"pad":"{pad}"}}"#))).unwrap();
        let bytes = long.secret().len();
        assert!(bytes > BLOB_CAP);
        let error = blob(&long).unwrap_err();
        assert_eq!(error, VaultError::TooLong { bytes });
        let said = error.to_string();
        assert!(said.contains("2560"), "{said}");
        assert!(!said.contains(long.secret()), "{said}");
    }

    #[test]
    fn a_store_error_names_the_call_and_what_the_store_said() {
        let error = VaultError::Store {
            doing: Doing::Write,
            message: "Platform failure: Windows error code 5".into(),
        };
        assert_eq!(
            error.to_string(),
            "writing the credential failed: Platform failure: Windows error code 5"
        );
        assert_eq!(Doing::Read.to_string(), "reading the credential");
        assert_eq!(Doing::Delete.to_string(), "deleting the credential");
    }
}
