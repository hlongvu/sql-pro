//! Saved connections persistence — SQL Pro's stand-in for TablePro's
//! per-store classes. Passwords stored here are a v0.1 shortcut; the real
//! port must move them into the platform secret store (Keychain / libsecret
//! / DPAPI), mirroring TablePro's KeychainHelper split.

use std::path::PathBuf;

use sqlpro_core::connection::ConnectionConfig;

pub fn load_connections() -> Vec<ConnectionConfig> {
    let path = connections_path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    serde_json::from_str(&text).unwrap_or_else(|e| {
        eprintln!("warning: could not parse {}: {e}", path.display());
        Vec::new()
    })
}

pub fn save_connections(connections: &[ConnectionConfig]) {
    let path = connections_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match serde_json::to_string_pretty(connections) {
        Ok(text) => {
            if let Err(e) = std::fs::write(&path, text) {
                eprintln!("warning: could not write {}: {e}", path.display());
            }
        }
        Err(e) => eprintln!("warning: could not serialize connections: {e}"),
    }
}

fn connections_path() -> PathBuf {
    let mut base = match std::env::var_os("HOME") {
        Some(home) => PathBuf::from(home),
        None => PathBuf::from("."),
    };
    // Mirrors TablePro's layout choice: app-scoped support directory.
    base.push(".local/share/sqlpro");
    base.push("connections.json");
    base
}
