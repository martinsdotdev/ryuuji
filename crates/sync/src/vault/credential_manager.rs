//! Tokens kept in Windows' Credential Manager, reached through `keyring-core`
//! and `windows-native-keyring-store`: one generic credential per service,
//! named `Ryuuji/<service tag>`, which the Credential Manager panel lists
//! under Windows Credentials and a person can remove there.
//!
//! The entry is built with its `persistence` modifier set to Local
//! (`CRED_PERSIST_LOCAL_MACHINE`): the credential survives logoff and a
//! reboot, which is the point of keeping it at all, and it stays on this
//! computer, where the store's default, Enterprise, would roam with a domain
//! profile and carry a personal token to every machine the person signs in
//! to. The blob is the token's UTF-8 bytes, written with `set_secret`: a blob
//! is capped at `CRED_MAX_CREDENTIAL_BLOB_SIZE` (2560 bytes), a JWT is ASCII,
//! and `set_password` would store UTF-16, two bytes a character, and halve
//! the room. Nothing here logs or formats the token; an error names the call
//! and what the store said.

use std::collections::HashMap;

use keyring_core::api::CredentialStoreApi;
use keyring_core::{Entry, Error};
use ryuuji_core::Service;
use windows_native_keyring_store::Store;

use crate::Token;
use crate::vault::{Doing, VaultError, blob, target_name};

/// Holds nothing between calls: every one goes to Credential Manager.
#[derive(Default)]
pub(crate) struct Tokens {}

impl Tokens {
    pub(crate) fn keep(&mut self, service: Service, token: Token) -> Result<(), VaultError> {
        write(&target_name(service), blob(&token)?)
    }

    pub(crate) fn forget(&mut self, service: Service) -> Result<(), VaultError> {
        delete(&target_name(service))
    }

    pub(crate) fn token(&self, service: Service) -> Result<Option<Token>, VaultError> {
        read(&target_name(service))
    }
}

/// The entry for the credential named `target`, built from a store of its
/// own rather than the crate's process-wide default, so nothing has to be
/// registered before the first call. With a `target` given, the service and
/// user the store would otherwise name the credential by go unused. Building
/// the entry touches Credential Manager not at all; the persistence applies
/// when the secret is written.
fn entry(target: &str) -> Result<Entry, Error> {
    let modifiers = HashMap::from([("target", target), ("persistence", "Local")]);
    Store::new()?.build("", "", Some(&modifiers))
}

/// Writes `blob` under `target`, replacing what was there.
fn write(target: &str, blob: &[u8]) -> Result<(), VaultError> {
    entry(target)
        .and_then(|entry| entry.set_secret(blob))
        .map_err(|error| store(Doing::Write, error))
}

/// The token under `target`, or `None` when there is no such credential.
fn read(target: &str) -> Result<Option<Token>, VaultError> {
    let bytes = match entry(target).and_then(|entry| entry.get_secret()) {
        Ok(bytes) => bytes,
        Err(Error::NoEntry) => return Ok(None),
        Err(error) => return Err(store(Doing::Read, error)),
    };
    let text = String::from_utf8(bytes).map_err(|_| VaultError::NotAToken)?;
    Token::parse(&text)
        .map(Some)
        .map_err(|_| VaultError::NotAToken)
}

/// Deletes the credential under `target`; one that is not there is no
/// error, since forgetting is about the state after.
fn delete(target: &str) -> Result<(), VaultError> {
    match entry(target).and_then(|entry| entry.delete_credential()) {
        Ok(()) | Err(Error::NoEntry) => Ok(()),
        Err(error) => Err(store(Doing::Delete, error)),
    }
}

fn store(doing: Doing, error: Error) -> VaultError {
    VaultError::Store {
        doing,
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::process;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::tests::token_until;
    use crate::vault::BLOB_CAP;

    /// A target name no run shares, deleted however the test ends.
    struct TestTarget(String);

    impl TestTarget {
        fn unique() -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |since| since.as_nanos());
            TestTarget(format!("Ryuuji-test/{}-{nanos}", process::id()))
        }
    }

    impl Drop for TestTarget {
        fn drop(&mut self) {
            let _ = delete(&self.0);
        }
    }

    /// The persistence Credential Manager reports for the credential under
    /// `target`, as the store spells it.
    fn persistence(target: &str) -> String {
        entry(target).unwrap().get_attributes().unwrap()["persistence"].clone()
    }

    /// The store refuses a blob past its own cap before anything reaches
    /// Credential Manager, so this writes nothing and runs everywhere; it
    /// holds `BLOB_CAP` to the store's limit.
    #[test]
    fn the_cap_is_the_stores_own() {
        let entry = entry("Ryuuji-test/cap").unwrap();
        let error = entry.set_secret(&vec![b'a'; BLOB_CAP + 1]).unwrap_err();
        assert!(
            matches!(&error, Error::TooLong(_, cap) if *cap as usize == BLOB_CAP),
            "{error}"
        );
    }

    /// Writes to this computer's Credential Manager, under the test target
    /// only: `cargo test -p ryuuji-sync -- --ignored`.
    #[test]
    #[ignore = "writes a credential to this computer's Credential Manager"]
    fn a_token_round_trips_through_credential_manager() {
        let target = TestTarget::unique();
        assert_eq!(read(&target.0), Ok(None));
        let token = token_until(1_790_121_600);
        write(&target.0, blob(&token).unwrap()).unwrap();
        assert_eq!(read(&target.0), Ok(Some(token)));
        assert_eq!(persistence(&target.0), "Local");
        let newer = token_until(1_800_000_000);
        write(&target.0, blob(&newer).unwrap()).unwrap();
        assert_eq!(read(&target.0), Ok(Some(newer)));
        assert_eq!(persistence(&target.0), "Local");
        delete(&target.0).unwrap();
        assert_eq!(read(&target.0), Ok(None));
        delete(&target.0).unwrap();
    }
}
