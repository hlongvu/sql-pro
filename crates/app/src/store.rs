//! Durable saved-connection metadata.
//!
//! Passwords are deliberately removed before serialization. Callers that want
//! persisted credentials must use the secret-store abstraction separately.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use sqlpro_core::connection::{ConnectionConfig, SshAuth};

use crate::storage;

pub type Result<T> = std::result::Result<T, StoreError>;

#[derive(Debug)]
pub enum StoreError {
    Io { path: PathBuf, source: io::Error },
    Serialize(serde_json::Error),
    Parse { path: PathBuf, source: serde_json::Error },
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(formatter, "could not access {}: {source}", path.display())
            }
            Self::Serialize(source) => write!(formatter, "could not serialize connections: {source}"),
            Self::Parse { path, source } => {
                write!(formatter, "could not parse {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Serialize(source) => Some(source),
            Self::Parse { source, .. } => Some(source),
        }
    }
}

/// Compatibility loader for the current workspace UI.
///
/// New callers should use [`try_load_connections`] so malformed or unreadable
/// storage can be surfaced to the user.
pub fn load_connections() -> Vec<ConnectionConfig> {
    match try_load_connections() {
        Ok(connections) => connections,
        Err(error) => {
            eprintln!("warning: {error}");
            Vec::new()
        }
    }
}

/// Load connection metadata, distinguishing a missing first-run file from
/// parse and I/O failures.
pub fn try_load_connections() -> Result<Vec<ConnectionConfig>> {
    load_connections_from(&connections_path())
}

/// Atomically persist connection metadata.
///
/// This signature remains source-compatible with callers that ignore the
/// return value, while allowing updated UI code to report failures.
#[must_use = "connection persistence failures should be surfaced to the user"]
pub fn save_connections(connections: &[ConnectionConfig]) -> Result<()> {
    save_connections_to(&connections_path(), connections)
}

fn load_connections_from(path: &Path) -> Result<Vec<ConnectionConfig>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(StoreError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    storage::harden_private_file(path).map_err(|source| StoreError::Io {
        path: path.to_path_buf(),
        source,
    })?;

    serde_json::from_str(&text).map_err(|source| StoreError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

fn save_connections_to(path: &Path, connections: &[ConnectionConfig]) -> Result<()> {
    let sanitized: Vec<_> = connections.iter().cloned().map(without_secrets).collect();
    let mut encoded = serde_json::to_vec_pretty(&sanitized).map_err(StoreError::Serialize)?;
    encoded.push(b'\n');
    storage::atomic_write(path, &encoded).map_err(|source| StoreError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn without_secrets(mut connection: ConnectionConfig) -> ConnectionConfig {
    connection.password = None;
    if let Some(tunnel) = &mut connection.ssh_tunnel {
        match &mut tunnel.auth {
            SshAuth::Password { password } => password.clear(),
            SshAuth::Key { passphrase, .. } => *passphrase = None,
        }
    }
    connection
}

fn connections_path() -> PathBuf {
    storage::app_data_dir().join("connections.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::test_support::TestDir;
    use sqlpro_core::connection::{SshAuth, SshTunnelConfig};
    use sqlpro_core::database_type::DatabaseType;

    fn connection_with_secrets() -> ConnectionConfig {
        let mut connection =
            ConnectionConfig::new("Local", DatabaseType::new(DatabaseType::POSTGRESQL));
        connection.password = Some("database-password".into());
        connection.ssh_tunnel = Some(SshTunnelConfig {
            host: "jump.example.com".into(),
            port: 22,
            username: "operator".into(),
            auth: SshAuth::Key {
                private_key_path: "/home/operator/.ssh/id_ed25519".into(),
                passphrase: Some("key-passphrase".into()),
            },
        });
        connection
    }

    #[test]
    fn missing_file_loads_as_empty() {
        let temporary = TestDir::new("connections-missing");
        let path = temporary.path().join("connections.json");

        assert!(load_connections_from(&path).unwrap().is_empty());
    }

    #[test]
    fn save_round_trips_metadata_without_plaintext_secrets() {
        let temporary = TestDir::new("connections-round-trip");
        let path = temporary.path().join("connections.json");
        let connection = connection_with_secrets();

        save_connections_to(&path, std::slice::from_ref(&connection)).unwrap();

        let raw = fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("database-password"));
        assert!(!raw.contains("key-passphrase"));
        let loaded = load_connections_from(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, connection.id);
        assert_eq!(loaded[0].name, connection.name);
        assert_eq!(loaded[0].password, None);
        assert_eq!(
            loaded[0]
                .ssh_tunnel
                .as_ref()
                .and_then(|tunnel| match &tunnel.auth {
                    SshAuth::Key { passphrase, .. } => passphrase.as_deref(),
                    SshAuth::Password { .. } => None,
                }),
            None
        );
    }

    #[test]
    fn malformed_json_is_reported() {
        let temporary = TestDir::new("connections-malformed");
        let path = temporary.path().join("connections.json");
        fs::write(&path, "not json").unwrap();

        assert!(matches!(
            load_connections_from(&path),
            Err(StoreError::Parse { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn existing_connection_file_is_hardened_on_load() {
        use std::os::unix::fs::PermissionsExt;

        let temporary = TestDir::new("connections-harden");
        let path = temporary.path().join("connections.json");
        fs::write(&path, "[]").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        load_connections_from(&path).unwrap();

        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }
}
