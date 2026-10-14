use crate::secret::Secret;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Path, PathBuf};

#[derive(Clone, Serialize, Deserialize)]
pub struct Indexer {
    pub id: u32,
    pub name: String,
    /// Torznab endpoint, e.g. `http://localhost:9117/api/v2.0/indexers/all/results/torznab/`.
    pub url: String,
    pub api_key: Secret,
    #[serde(default = "yes")]
    pub enabled: bool,
}

impl fmt::Debug for Indexer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Indexer")
            .field("id", &self.id)
            .field("enabled", &self.enabled)
            .finish_non_exhaustive()
    }
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FetchConfig {
    #[serde(default)]
    pub indexers: Vec<Indexer>,
    #[serde(default)]
    pub download_dir: Option<PathBuf>,
    #[serde(default = "yes")]
    pub dht: bool,
    /// Accept incoming peer connections. Off: outgoing connections only.
    #[serde(default)]
    pub listen_incoming: bool,
    #[serde(default)]
    pub seed_after_complete: bool,
    /// `socks5://[user:pass@]host:port`; every outgoing peer connection goes
    /// through it when set.
    #[serde(default)]
    pub proxy: Option<Secret>,
}

impl Default for FetchConfig {
    fn default() -> Self {
        Self {
            indexers: Vec::new(),
            download_dir: None,
            dht: true,
            listen_incoming: false,
            seed_after_complete: false,
            proxy: None,
        }
    }
}

impl FetchConfig {
    pub fn default_path() -> PathBuf {
        jizzcraft_core::paths::config_dir().join("fetch.json")
    }

    pub fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    /// Written to a temp file with mode 0600, then renamed over the target.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        use std::io::Write;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(&serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    }

    pub fn next_indexer_id(&self) -> u32 {
        self.indexers
            .iter()
            .map(|i| i.id)
            .max()
            .map_or(1, |m| m + 1)
    }
}
