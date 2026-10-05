use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{Error, Result};

/// A connection's durable identity. Passwords live outside this struct in
/// the platform secret store (macOS Keychain, libsecret, DPAPI), mirroring
/// TablePro's split between `DatabaseConnection` and `KeychainHelper`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionConfig {
    pub id: Uuid,
    pub name: String,
    pub database_type: crate::database_type::DatabaseType,
    /// Hostname or IP for TCP engines; unused for SQLite.
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    /// File path for file-backed engines (SQLite).
    #[serde(default)]
    pub path: Option<String>,
    /// Database name inside the server, if the engine distinguishes it.
    #[serde(default)]
    pub database: Option<String>,
    #[serde(default)]
    pub ssl_mode: SslMode,
    #[serde(default)]
    pub ssh_tunnel: Option<SshTunnelConfig>,
    #[serde(default = "default_query_timeout")]
    pub query_timeout_secs: u64,
}

fn default_query_timeout() -> u64 {
    30
}

impl ConnectionConfig {
    pub fn new(
        name: impl Into<String>,
        database_type: crate::database_type::DatabaseType,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            database_type,
            host: None,
            port: None,
            username: None,
            password: None,
            path: None,
            database: None,
            ssl_mode: SslMode::Prefer,
            ssh_tunnel: None,
            query_timeout_secs: default_query_timeout(),
        }
    }

    pub fn port(&self) -> Option<u16> {
        self.port.or_else(|| self.database_type.default_port())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SslMode {
    Disable,
    #[default]
    Prefer,
    Require,
    VerifyFull,
}

/// SSH tunnel configuration, mirroring TablePro's `SSHConnection` subset.
/// libssh2-based tunneling is a later milestone; the model exists now so
/// saved connections already round-trip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SshTunnelConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: SshAuth,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SshAuth {
    Password { password: String },
    Key { private_key_path: String, passphrase: Option<String> },
}

/// Validate a config before connecting, like TablePro's connection form
/// validation.
pub fn validate(config: &ConnectionConfig) -> Result<()> {
    match config.database_type.driver_id() {
        "sqlite" => {
            if config.path.is_none() {
                return Err(Error::Connection("SQLite requires a file path".into()));
            }
        }
        _ => {
            if config.host.is_none() {
                return Err(Error::Connection("host is required".into()));
            }
            if config.port().is_none() {
                return Err(Error::Connection("port is required".into()));
            }
        }
    }
    Ok(())
}
