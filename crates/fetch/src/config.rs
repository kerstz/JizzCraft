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

/// What to do with torrents left unfinished by the previous run.
/// Nothing restarts silently: the default asks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResumeOnStart {
    /// Restored torrents stay paused and the user is not asked.
    Never,
    /// Restored torrents stay paused; the UI offers "Resume all".
    #[default]
    Ask,
    /// Restored torrents continue by themselves.
    Always,
}

impl ResumeOnStart {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "never" => Some(Self::Never),
            "ask" => Some(Self::Ask),
            "always" => Some(Self::Always),
            _ => None,
        }
    }
}

/// Preferences of the optional browser helper (online edition only). Kept
/// here so they live in `fetch.json` next to the proxy; the offline build
/// never reads them. Nothing in it is a secret.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BrowserPrefs {
    /// The browser button and `Ctrl+Shift+B` are available.
    pub enabled: bool,
    /// Keep sign-ins (cookies, site data) between runs. Off by default.
    pub remember: bool,
    pub js: bool,
    /// Video inside pages.
    pub media: bool,
    /// The user's own content-rule JSON file (path only).
    pub rules_path: Option<String>,
    /// Search address pattern with one `{}`. Empty: no search engine.
    pub search_template: Option<String>,
    /// Software rendering.
    pub software: bool,
}

impl Default for BrowserPrefs {
    fn default() -> Self {
        Self {
            enabled: true,
            remember: false,
            js: true,
            media: true,
            rules_path: None,
            search_template: None,
            software: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FetchConfig {
    #[serde(default)]
    pub indexers: Vec<Indexer>,
    /// User-defined streaming sources. The app ships none.
    #[serde(default)]
    pub sources: Vec<crate::sources::SourceDef>,
    /// Programs the user installed that turn a page address into a file
    /// address. The app ships none.
    #[cfg(feature = "network")]
    #[serde(default)]
    pub resolvers: Vec<crate::resolver::ResolverDef>,
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
    /// Kill switch: nothing connects unless a proxy is set (use it with a
    /// VPN or Tor SOCKS endpoint so a dropped proxy never falls back to the
    /// direct connection).
    // Fail closed: a door that sticks is better than one that swings open.
    #[serde(default)]
    pub require_proxy: bool,
    /// Speed caps in MiB/s (0 or absent = unlimited).
    #[serde(default)]
    pub down_limit_mibps: Option<u32>,
    #[serde(default)]
    pub up_limit_mibps: Option<u32>,
    /// Restored torrents after a restart (default: ask).
    #[serde(default)]
    pub resume_on_start: ResumeOnStart,
    /// With seeding on: stop at this upload/size ratio (absent = never).
    #[serde(default)]
    pub seed_ratio: Option<f32>,
    /// With seeding on: stop this many minutes after completion (absent = never).
    #[serde(default)]
    pub seed_minutes: Option<u32>,
    /// Network interface (e.g. a VPN's `tun0`) that DHT, trackers and peer
    /// connections are bound to. If it is missing the session does not start.
    #[serde(default)]
    pub bind_interface: Option<String>,
    /// Browser helper preferences (online edition).
    #[serde(default)]
    pub browser: BrowserPrefs,
    /// Runtime only: the file on disk was unreadable and was set aside as
    /// `fetch.json.corrupt-<unixtime>`; defaults are in use. Never saved.
    #[serde(skip)]
    pub load_error: bool,
    /// Runtime only: neutral names (folder `Viewer`) while the window is
    /// disguised. Set by the app, never saved.
    #[serde(skip)]
    pub disguised: bool,
}

impl Default for FetchConfig {
    fn default() -> Self {
        Self {
            indexers: Vec::new(),
            sources: Vec::new(),
            #[cfg(feature = "network")]
            resolvers: Vec::new(),
            download_dir: None,
            dht: true,
            listen_incoming: false,
            seed_after_complete: false,
            proxy: None,
            require_proxy: false,
            down_limit_mibps: None,
            up_limit_mibps: None,
            resume_on_start: ResumeOnStart::Ask,
            seed_ratio: None,
            seed_minutes: None,
            bind_interface: None,
            browser: BrowserPrefs::default(),
            load_error: false,
            disguised: false,
        }
    }
}

/// What the torrent session may use. A SOCKS5 proxy only carries TCP peer
/// connections: DHT and trackers cannot go through it, and an incoming
/// listener would expose the real address. So with a proxy configured they
/// are switched off rather than silently leaking.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NetPolicy {
    pub dht: bool,
    pub trackers: bool,
    pub listen: bool,
    /// UPnP / NAT-PMP port mapping. Always off: it would publish the machine
    /// to the router, and there is no setting to turn it on.
    pub upnp: bool,
}

impl FetchConfig {
    pub fn net_policy(&self) -> NetPolicy {
        let proxied = self.proxy.as_ref().is_some_and(|p| !p.is_empty());
        NetPolicy {
            dht: self.dht && !proxied,
            trackers: !proxied,
            listen: self.listen_incoming && !proxied,
            upnp: false,
        }
    }
}

impl FetchConfig {
    pub fn default_path() -> PathBuf {
        jizzcraft_core::paths::config_dir().join("fetch.json")
    }

    /// Read the file. Missing: defaults. Unreadable or corrupt: the file is
    /// renamed to `fetch.json.corrupt-<unixtime>` (keys inside survive for
    /// the user to recover), defaults are returned and `load_error` is set.
    /// On Unix a file that is group/world accessible is fixed to 0600.
    pub fn load(path: &Path) -> Self {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(_) => {
                return Self {
                    load_error: true,
                    ..Self::default()
                };
            }
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(m) = std::fs::metadata(path)
                && m.permissions().mode() & 0o077 != 0
            {
                let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
            }
        }
        match serde_json::from_slice::<Self>(&bytes) {
            Ok(c) => c,
            Err(_) => {
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs());
                // Never replace an earlier set-aside copy: it may hold keys too.
                let mut dest = None;
                for n in 0..100u32 {
                    let mut name = path.as_os_str().to_owned();
                    name.push(if n == 0 {
                        format!(".corrupt-{ts}")
                    } else {
                        format!(".corrupt-{ts}-{n}")
                    });
                    let p = PathBuf::from(name);
                    if !p.exists() {
                        dest = Some(p);
                        break;
                    }
                }
                if let Some(d) = dest {
                    let _ = std::fs::rename(path, d);
                }
                Self {
                    load_error: true,
                    ..Self::default()
                }
            }
        }
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
        jizzcraft_core::fsutil::replace(&tmp, path)
    }

    /// Where downloads go. Nothing to configure: the chosen folder if there
    /// is one, else `<Downloads>/JizzCraft` (`Viewer` while disguised), else
    /// `downloads` inside the portable home. Never created here.
    pub fn effective_download_dir(&self) -> PathBuf {
        if let Some(d) = &self.download_dir {
            return d.clone();
        }
        if let Some(h) = jizzcraft_core::paths::home() {
            return h.join("downloads");
        }
        let leaf = if self.disguised {
            "Viewer"
        } else {
            "JizzCraft"
        };
        directories::UserDirs::new()
            .and_then(|u| u.download_dir().map(|p| p.join(leaf)))
            .unwrap_or_else(|| jizzcraft_core::paths::state_dir().join("downloads"))
    }

    pub fn next_indexer_id(&self) -> u32 {
        self.indexers
            .iter()
            .map(|i| i.id)
            .max()
            .map_or(1, |m| m + 1)
    }
}
