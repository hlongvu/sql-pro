use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::database_type::DatabaseType;
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
    /// Status color swatch (TablePlus-style), an RGB value like 0x6EC878.
    #[serde(default)]
    pub color: Option<u32>,
    /// Free-form environment tag ("local", "prod", …) shown as a chip.
    #[serde(default)]
    pub tag: Option<String>,
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
            color: None,
            tag: None,
        }
    }

    pub fn port(&self) -> Option<u16> {
        self.port.or_else(|| self.database_type.default_port())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
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

/// The parts of a connection URL, ready to pour into a form / config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedConnectionUrl {
    pub database_type: DatabaseType,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub database: Option<String>,
    pub ssl_mode: Option<SslMode>,
}

/// Parse a connection URL ("adding by connection string"): the scheme picks
/// the engine, the authority carries user/password/host/port, the path is
/// the database, and `?sslmode=` maps to [`SslMode`]. Handles the common
/// scheme aliases (postgres/postgresql, mongo/mongodb, mssql/sqlserver,
/// cockroach/cockroachdb, …). Percent-decodes userinfo and database.
pub fn parse_connection_url(url: &str) -> std::result::Result<ParsedConnectionUrl, String> {
    let url = url.trim();
    let Some((scheme, rest)) = url.split_once("://") else {
        return Err(
            "Not a connection URL — expected scheme://[user:pass@]host[:port][/database]".into(),
        );
    };
    let database_type = match scheme.to_lowercase().as_str() {
        "postgres" | "postgresql" => DatabaseType::new(DatabaseType::POSTGRESQL),
        "mysql" => DatabaseType::new(DatabaseType::MYSQL),
        "mariadb" => DatabaseType::new("mariadb"),
        "sqlite" | "sqlite3" | "file" => DatabaseType::new(DatabaseType::SQLITE),
        "mssql" | "sqlserver" => DatabaseType::new("mssql"),
        "mongo" | "mongodb" => DatabaseType::new("mongodb"),
        "redis" | "rediss" => DatabaseType::new("redis"),
        "cassandra" | "scylla" => DatabaseType::new("cassandra"),
        "snowflake" => DatabaseType::new("snowflake"),
        "oracle" => DatabaseType::new("oracle"),
        "vertica" => DatabaseType::new("vertica"),
        "cockroach" | "cockroachdb" => DatabaseType::new("cockroachdb"),
        "redshift" => DatabaseType::new("redshift"),
        other => return Err(format!("Unsupported URL scheme \"{other}://\"")),
    };

    // SQLite is file-backed: everything after the scheme is the path, no
    // authority to split (`sqlite:///path/to/db.sqlite`, `sqlite://:memory:`).
    if database_type.id() == DatabaseType::SQLITE {
        let path = percent_decode(rest.split('?').next().unwrap_or(rest));
        return Ok(ParsedConnectionUrl {
            database_type,
            host: None,
            port: None,
            username: None,
            password: None,
            database: (!path.is_empty()).then_some(path),
            ssl_mode: None,
        });
    }

    // Split off the query string, keeping `sslmode=` if present.
    let (authority_path, query) = match rest.split_once('?') {
        Some((a, q)) => (a, Some(q)),
        None => (rest, None),
    };
    let ssl_mode = query.and_then(|q| {
        q.split('&').find_map(|param| match param.split_once('=') {
            Some(("sslmode", v)) => match v.to_lowercase().as_str() {
                "disable" | "disabled" => Some(SslMode::Disable),
                "prefer" | "preferred" => Some(SslMode::Prefer),
                "require" | "required" => Some(SslMode::Require),
                "verify-ca" | "verify-full" => Some(SslMode::VerifyFull),
                _ => None,
            },
            _ => None,
        })
    });

    let (authority, path) = match authority_path.split_once('/') {
        Some((a, p)) => (a, Some(p)),
        None => (authority_path, None),
    };

    // userinfo@hostport — split on the LAST '@' so decoded passwords may
    // still contain one after percent-decoding.
    let (username, password, hostport) = match authority.rsplit_once('@') {
        Some((userinfo, hostport)) => {
            let (u, p) = match userinfo.split_once(':') {
                Some((u, p)) => (
                    (!u.is_empty()).then(|| percent_decode(u)),
                    (!p.is_empty()).then(|| percent_decode(p)),
                ),
                None => ((!userinfo.is_empty()).then(|| percent_decode(userinfo)), None),
            };
            (u, p, hostport)
        }
        None => (None, None, authority),
    };

    // host[:port], with [IPv6] bracket support.
    let (host, port) = split_host_port(hostport)?;

    let database = path
        .map(percent_decode)
        .filter(|db| !db.is_empty())
        .map(|db| db.trim_end_matches('/').to_string());

    Ok(ParsedConnectionUrl {
        database_type,
        host: Some(host),
        port,
        username,
        password,
        database,
        ssl_mode,
    })
}

/// Split `host[:port]`, treating `[::1]:5432`-style brackets as IPv6.
fn split_host_port(authority: &str) -> std::result::Result<(String, Option<u16>), String> {
    let (host, port_str) = if let Some(rest) = authority.strip_prefix('[') {
        let Some(bracket_end) = rest.find(']') else {
            return Err("Unterminated [ in host".into());
        };
        let host = &rest[..bracket_end];
        let after = &rest[bracket_end + 1..];
        let port = after.strip_prefix(':');
        (host.to_string(), port.map(str::to_string))
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), Some(p.to_string())),
            None => (authority.to_string(), None),
        }
    };
    let port = match port_str {
        Some(p) if !p.is_empty() => Some(p.parse().map_err(|_| format!("Bad port \"{p}\""))?),
        _ => None,
    };
    Ok((host, port))
}

/// Decode `%XX` sequences; invalid escapes pass through unchanged.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(
                std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""),
                16,
            ) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_postgres_url_with_everything() {
        let p = parse_connection_url(
            "postgres://bob:secret@db.example.com:5433/sales?sslmode=require",
        )
        .unwrap();
        assert_eq!(p.database_type.id(), "postgresql");
        assert_eq!(p.host.as_deref(), Some("db.example.com"));
        assert_eq!(p.port, Some(5433));
        assert_eq!(p.username.as_deref(), Some("bob"));
        assert_eq!(p.password.as_deref(), Some("secret"));
        assert_eq!(p.database.as_deref(), Some("sales"));
        assert_eq!(p.ssl_mode, Some(SslMode::Require));
    }

    #[test]
    fn parse_minimal_and_aliased_schemes() {
        let p = parse_connection_url("postgresql://localhost/mydb").unwrap();
        assert_eq!(p.database_type.id(), "postgresql");
        assert_eq!(p.host.as_deref(), Some("localhost"));
        assert_eq!(p.port, None);
        assert_eq!(p.database.as_deref(), Some("mydb"));

        let p = parse_connection_url("mariadb://root@127.0.0.1/world").unwrap();
        assert_eq!(p.database_type.id(), "mariadb");
        assert_eq!(p.username.as_deref(), Some("root"));

        let p = parse_connection_url("mongodb://u:p@host:27017").unwrap();
        assert_eq!(p.database_type.id(), "mongodb");
        assert_eq!(p.port, Some(27017));
        assert_eq!(p.database, None);

        let p = parse_connection_url("redis://:seekrit@localhost:6379/0").unwrap();
        assert_eq!(p.database_type.id(), "redis");
        assert_eq!(p.username, None);
        assert_eq!(p.password.as_deref(), Some("seekrit"));
        assert_eq!(p.database.as_deref(), Some("0"));
    }

    #[test]
    fn parse_sqlite_path_and_memory() {
        let p = parse_connection_url("sqlite:///Users/longvu/data/app.db").unwrap();
        assert_eq!(p.database_type.id(), "sqlite");
        assert_eq!(p.database.as_deref(), Some("/Users/longvu/data/app.db"));

        let p = parse_connection_url("sqlite://:memory:").unwrap();
        assert_eq!(p.database.as_deref(), Some(":memory:"));
    }

    #[test]
    fn percent_decoding_and_ipv6() {
        let p = parse_connection_url("postgres://bob:p%40ss%21@10.0.0.9/d%62").unwrap();
        assert_eq!(p.username.as_deref(), Some("bob"));
        assert_eq!(p.password.as_deref(), Some("p@ss!"));
        assert_eq!(p.database.as_deref(), Some("db"));

        let p = parse_connection_url("postgresql://u@[::1]:5432/x").unwrap();
        assert_eq!(p.host.as_deref(), Some("::1"));
        assert_eq!(p.port, Some(5432));
    }

    #[test]
    fn rejects_non_urls_and_unknown_schemes() {
        assert!(parse_connection_url("127.0.0.1:5432").is_err());
        assert!(parse_connection_url("ftp://host/db").is_err());
    }
}
