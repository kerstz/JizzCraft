//! Thumbnails, codec probing and the GStreamer player.
//! Local files only: pipelines get `file://` URIs, and `guard` keeps every
//! network-capable GStreamer element out of them.

pub mod doctor;
pub mod guard;
pub mod integrity;
pub mod player;
pub mod preview;
pub mod probe;
pub mod service;
pub mod stream;
pub mod thumbs;

use std::sync::OnceLock;

/// Initialise GStreamer once, with network-capable elements taken out of this
/// process's registry. Safe to call from any thread, any number of times.
pub fn init() -> Result<(), String> {
    static INIT: OnceLock<Result<(), String>> = OnceLock::new();
    INIT.get_or_init(|| {
        gstreamer::init().map_err(|e| {
            format!("GStreamer is not usable: {e}. Install gstreamer + gst-plugins-base.")
        })?;
        scan_bundled_plugins();
        guard::harden_registry();
        Ok(())
    })
    .clone()
}

/// Plugin folders shipped with a packaged build, relative to the executable.
/// Windows zip: `gstreamer/lib/gstreamer-1.0`; macOS bundle:
/// `../Resources/gstreamer/lib/gstreamer-1.0`; relocatable Linux tree:
/// `../lib/gstreamer-1.0`. A system install needs none of this.
fn bundled_plugin_dirs(exe_dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    [
        "gstreamer/lib/gstreamer-1.0",
        "../Resources/gstreamer/lib/gstreamer-1.0",
        "../lib/gstreamer-1.0",
    ]
    .iter()
    .map(|r| exe_dir.join(r))
    .filter(|p| p.is_dir())
    .collect()
}

/// What a packaged build changes in the environment so that only its own
/// GStreamer is used: `(variable, Some(value))` to set, `(variable, None)` to
/// remove. Empty when this is not a package (a system install is left alone).
/// Pure, so it can be tested everywhere.
pub fn bundled_env(
    exe_dir: &std::path::Path,
    cache_dir: &std::path::Path,
) -> Vec<(&'static str, Option<std::ffi::OsString>)> {
    let Some(plugins) = bundled_plugin_dirs(exe_dir).into_iter().next() else {
        return Vec::new();
    };
    let mut v: Vec<(&'static str, Option<std::ffi::OsString>)> = vec![
        // Only the packaged plugins: a GStreamer installed by the user (and
        // its own variables) must never be mixed with these libraries.
        ("GST_PLUGIN_SYSTEM_PATH_1_0", Some(plugins.into_os_string())),
        ("GST_PLUGIN_PATH_1_0", None),
        ("GST_PLUGIN_PATH", None),
        (
            "GST_REGISTRY_1_0",
            Some(cache_dir.join("gst-registry.bin").into_os_string()),
        ),
    ];
    let exe = if cfg!(windows) {
        "gst-plugin-scanner.exe"
    } else {
        "gst-plugin-scanner"
    };
    for rel in [
        exe.to_owned(),
        format!("gstreamer/libexec/gstreamer-1.0/{exe}"),
        format!("../libexec/gstreamer-1.0/{exe}"),
    ] {
        let p = exe_dir.join(rel);
        if p.is_file() {
            v.push(("GST_PLUGIN_SCANNER_1_0", Some(p.into_os_string())));
            break;
        }
    }
    v
}

/// Isolate a packaged build from any GStreamer already on the machine. Call
/// it first thing in `main`, before any thread starts: changing the
/// environment is only sound while the process is single-threaded.
pub fn prepare_bundled_env() {
    let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(std::path::Path::to_path_buf))
    else {
        return;
    };
    let cache = jizzcraft_core::paths::cache_dir();
    let _ = jizzcraft_core::fsutil::create_private_dir_all(&cache);
    for (k, v) in bundled_env(&dir, &cache) {
        // SAFETY: called from `main` before any other thread exists.
        unsafe {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
    }
}

/// Add packaged plugins to this process's registry.
/// Runs before `harden_registry`, so network elements shipped in a bundle are
/// taken out like any other.
fn scan_bundled_plugins() {
    let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(std::path::Path::to_path_buf))
    else {
        return;
    };
    for p in bundled_plugin_dirs(&dir) {
        let _ = gstreamer::Registry::get().scan_path(&p);
    }
}

/// A `file://` URI for a local path. Never produces anything else.
pub fn file_uri(path: &std::path::Path) -> Result<String, String> {
    let abs =
        jizzcraft_core::pathbytes::canonical(path).map_err(|e| format!("cannot open file: {e}"))?;
    let uri = gstreamer::glib::filename_to_uri(&abs, None)
        .map(|u| u.to_string())
        .map_err(|e| format!("bad path: {e}"))?;
    if uri.starts_with("file:///") {
        Ok(uri)
    } else {
        Err("not a local file".into())
    }
}

/// Turn a GStreamer error/missing-plugin message into a human sentence.
pub fn describe_missing_plugin(msg: &gstreamer::Message) -> Option<String> {
    use gstreamer_pbutils::MissingPluginMessage;
    if !MissingPluginMessage::is(msg) {
        return None;
    }
    let m = MissingPluginMessage::parse(msg).ok()?;
    // Manifest demuxers are disabled on purpose: installing plugins won't help.
    // Neither will reinstalling the OS, but people will try.
    if guard::is_manifest_detail(&m.installer_detail()) {
        return Some(guard::REFUSED.into());
    }
    Some(format!(
        "missing GStreamer plugin: {} — install gst-plugins-good/bad/ugly and gst-libav",
        m.description()
    ))
}

/// Append a GStreamer debug string to an error only when debug logging is on
/// (it can contain file paths).
pub(crate) fn debug_detail(text: &mut String, detail: &str) {
    if jizzcraft_core::privacy::debug_enabled() {
        text.push_str(" (");
        text.push_str(detail);
        text.push(')');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packaged_build_gets_an_isolated_environment() {
        let d = tempfile::tempdir().unwrap();
        // Not a package: nothing is touched.
        assert!(bundled_env(d.path(), d.path()).is_empty());
        let plugins = d.path().join("gstreamer/lib/gstreamer-1.0");
        std::fs::create_dir_all(&plugins).unwrap();
        let env = bundled_env(d.path(), std::path::Path::new("/cache"));
        let get = |k: &str| env.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone());
        assert_eq!(
            get("GST_PLUGIN_SYSTEM_PATH_1_0"),
            Some(Some(plugins.into_os_string()))
        );
        assert_eq!(get("GST_PLUGIN_PATH_1_0"), Some(None));
        assert!(matches!(get("GST_REGISTRY_1_0"), Some(Some(_))));
    }

    #[test]
    fn bundled_plugin_dirs_are_found_next_to_the_executable() {
        let d = tempfile::tempdir().unwrap();
        assert!(bundled_plugin_dirs(d.path()).is_empty());
        std::fs::create_dir_all(d.path().join("gstreamer/lib/gstreamer-1.0")).unwrap();
        let found = bundled_plugin_dirs(d.path());
        assert_eq!(found.len(), 1);
        assert!(found[0].ends_with("gstreamer/lib/gstreamer-1.0"));
    }
}
