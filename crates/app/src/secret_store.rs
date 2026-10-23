//! Credential storage abstraction.
//!
//! This build has no OS credential-store dependency, so it intentionally uses
//! a process-local fallback. Secrets can be used for the current session but
//! are never serialized to disk, and callers can clearly report that durable
//! persistence is unavailable.

// The full store trait (OS variants, SSH key material) is the contract future
// connection-dialog work consumes; the session fallback is what ships today.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Persistence {
    OsSecretStore,
    SessionOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SecretKind {
    DatabasePassword,
    SshPassword,
    SshKeyPassphrase,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SecretKey {
    connection_id: Uuid,
    kind: SecretKind,
}

impl SecretKey {
    pub fn new(connection_id: Uuid, kind: SecretKind) -> Self {
        Self {
            connection_id,
            kind,
        }
    }

    pub fn database_password(connection_id: Uuid) -> Self {
        Self::new(connection_id, SecretKind::DatabasePassword)
    }

    pub fn ssh_password(connection_id: Uuid) -> Self {
        Self::new(connection_id, SecretKind::SshPassword)
    }

    pub fn ssh_key_passphrase(connection_id: Uuid) -> Self {
        Self::new(connection_id, SecretKind::SshKeyPassphrase)
    }
}

#[derive(Debug)]
pub enum SecretStoreError {
    Unavailable(&'static str),
    Internal(&'static str),
}

impl std::fmt::Display for SecretStoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(message) | Self::Internal(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for SecretStoreError {}

pub trait SecretStore: Send + Sync {
    fn persistence(&self) -> Persistence;
    fn get(&self, key: &SecretKey) -> Result<Option<String>, SecretStoreError>;
    fn set(&self, key: SecretKey, secret: String) -> Result<(), SecretStoreError>;
    fn delete(&self, key: &SecretKey) -> Result<(), SecretStoreError>;

    fn availability_message(&self) -> Option<&'static str> {
        match self.persistence() {
            Persistence::OsSecretStore => None,
            Persistence::SessionOnly => Some(SESSION_ONLY_MESSAGE),
        }
    }
}

pub const SESSION_ONLY_MESSAGE: &str =
    "OS credential persistence is unavailable in this build; passwords are kept only for this session.";

/// Return the best credential store available in this build.
///
/// The current dependency set does not expose Keychain, Credential Manager, or
/// Secret Service APIs. Returning the explicit fallback is safer than writing
/// plaintext credentials into connection JSON.
pub fn platform_secret_store() -> Arc<dyn SecretStore> {
    Arc::new(SessionSecretStore::default())
}

#[derive(Debug, Clone, Default)]
pub struct SessionSecretStore {
    secrets: Arc<Mutex<HashMap<SecretKey, String>>>,
}

impl SecretStore for SessionSecretStore {
    fn persistence(&self) -> Persistence {
        Persistence::SessionOnly
    }

    fn get(&self, key: &SecretKey) -> Result<Option<String>, SecretStoreError> {
        Ok(self
            .secrets
            .lock()
            .map_err(|_| SecretStoreError::Internal("session secret store lock was poisoned"))?
            .get(key)
            .cloned())
    }

    fn set(&self, key: SecretKey, secret: String) -> Result<(), SecretStoreError> {
        self.secrets
            .lock()
            .map_err(|_| SecretStoreError::Internal("session secret store lock was poisoned"))?
            .insert(key, secret);
        Ok(())
    }

    fn delete(&self, key: &SecretKey) -> Result<(), SecretStoreError> {
        self.secrets
            .lock()
            .map_err(|_| SecretStoreError::Internal("session secret store lock was poisoned"))?
            .remove(key);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_is_explicitly_session_only() {
        let store = platform_secret_store();

        assert_eq!(store.persistence(), Persistence::SessionOnly);
        assert_eq!(store.availability_message(), Some(SESSION_ONLY_MESSAGE));
    }

    #[test]
    fn session_store_supports_get_set_and_delete() {
        let store = SessionSecretStore::default();
        let key = SecretKey::database_password(Uuid::new_v4());

        assert_eq!(store.get(&key).unwrap(), None);
        store.set(key.clone(), "secret".into()).unwrap();
        assert_eq!(store.get(&key).unwrap().as_deref(), Some("secret"));
        store.delete(&key).unwrap();
        assert_eq!(store.get(&key).unwrap(), None);
    }

    #[test]
    fn separate_instances_do_not_share_secrets() {
        let key = SecretKey::ssh_password(Uuid::new_v4());
        let first = SessionSecretStore::default();
        let second = SessionSecretStore::default();

        first.set(key.clone(), "secret".into()).unwrap();

        assert_eq!(second.get(&key).unwrap(), None);
    }
}
