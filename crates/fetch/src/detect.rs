//! "Find on this computer": look for a local Jackett or Prowlarr on loopback
//! and read the API key from its own config file. Runs only when the user
//! clicks the button, only on 127.0.0.1, only on the two well-known ports.
//! Knocking on your own front door, twice, politely.

use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Jackett,
    Prowlarr,
}

/// One endpoint ready to be added as an indexer.
#[derive(Clone, Debug)]
pub struct Detected {
    pub name: String,
    pub url: String,
    pub api_key: String,
}

pub const JACKETT_PORT: u16 = 9117;
pub const PROWLARR_PORT: u16 = 9696;

fn is_listening(port: u16) -> bool {
    let a = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&a, Duration::from_millis(500)).is_ok()
}

/// Jackett: `ServerConfig.json` with `"APIKey": "…"`.
pub fn jackett_key(config_dir: &Path) -> Option<String> {
    let txt = std::fs::read_to_string(config_dir.join("Jackett/ServerConfig.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&txt).ok()?;
    v.get("APIKey")?
        .as_str()
        .map(str::to_owned)
        .filter(|k| !k.is_empty())
}

/// Prowlarr: `config.xml` with `<ApiKey>…</ApiKey>`.
pub fn prowlarr_key(config_dir: &Path) -> Option<String> {
    let txt = std::fs::read_to_string(config_dir.join("Prowlarr/config.xml")).ok()?;
    let a = txt.find("<ApiKey>")? + "<ApiKey>".len();
    let b = txt[a..].find("</ApiKey>")? + a;
    Some(txt[a..b].trim().to_owned()).filter(|k| !k.is_empty())
}

pub fn user_config_dir() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|d| d.config_dir().to_path_buf())
}

/// Which services answer on loopback. Ports are parameters so tests can use
/// ephemeral ones.
pub fn listening(jackett_port: u16, prowlarr_port: u16) -> Vec<(Kind, u16)> {
    let mut v = Vec::new();
    if is_listening(jackett_port) {
        v.push((Kind::Jackett, jackett_port));
    }
    if is_listening(prowlarr_port) {
        v.push((Kind::Prowlarr, prowlarr_port));
    }
    v
}

/// Jackett exposes one aggregate Torznab endpoint.
pub fn jackett_endpoint(port: u16, key: String) -> Detected {
    Detected {
        name: "Jackett".into(),
        url: format!("http://127.0.0.1:{port}/api/v2.0/indexers/all/results/torznab/"),
        api_key: key,
    }
}

/// Prowlarr exposes one Torznab endpoint per indexer: `/{id}/api`.
pub fn prowlarr_endpoint(port: u16, id: u64, key: String) -> Detected {
    Detected {
        name: format!("Prowlarr {id}"),
        url: format!("http://127.0.0.1:{port}/{id}"),
        api_key: key,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_keys_from_the_local_config_files() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("Jackett")).unwrap();
        std::fs::create_dir_all(d.path().join("Prowlarr")).unwrap();
        std::fs::write(
            d.path().join("Jackett/ServerConfig.json"),
            r#"{"APIKey":"jk1"}"#,
        )
        .unwrap();
        std::fs::write(
            d.path().join("Prowlarr/config.xml"),
            "<Config><ApiKey> pk2 </ApiKey></Config>",
        )
        .unwrap();
        assert_eq!(jackett_key(d.path()).as_deref(), Some("jk1"));
        assert_eq!(prowlarr_key(d.path()).as_deref(), Some("pk2"));
        assert_eq!(jackett_key(Path::new("/nonexistent")), None);
    }

    #[test]
    fn only_listening_ports_are_reported() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let open = l.local_addr().unwrap().port();
        let closed = {
            let t = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            t.local_addr().unwrap().port()
        };
        assert_eq!(listening(open, closed), vec![(Kind::Jackett, open)]);
        assert!(listening(closed, closed).is_empty());
    }
}
