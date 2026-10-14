//! What the window calls itself. Chosen once at launch: the app ID cannot
//! change while the app runs, and on Wayland the dock takes its name and icon
//! from the installed `.desktop` file whose name matches the app ID.

pub const APP_ID: &str = "org.jizzcraft.JizzCraft";
/// Neutral, and not under a domain anyone owns.
pub const DISGUISE_APP_ID: &str = "local.Viewer";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Identity {
    pub app_id: &'static str,
    /// Window title.
    pub title: &'static str,
    /// Name handed to the toolkit.
    pub app_name: &'static str,
    pub disguised: bool,
}

/// `cfg_disguise` is the saved setting, `flag` is `--disguise`.
pub fn launch_identity(cfg_disguise: bool, flag: bool) -> Identity {
    if cfg_disguise || flag {
        Identity {
            app_id: DISGUISE_APP_ID,
            title: "Viewer",
            app_name: "Viewer",
            disguised: true,
        }
    } else {
        Identity {
            app_id: APP_ID,
            title: "JizzCraft",
            app_name: "JizzCraft",
            disguised: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disguise_leaks_no_brand() {
        let d = launch_identity(true, false);
        assert!(d.disguised);
        for s in [d.app_id, d.title, d.app_name] {
            assert!(!s.to_lowercase().contains("jizz"), "{s}");
        }
        assert_eq!(launch_identity(false, true), d);
    }

    #[test]
    fn default_identity_is_unchanged() {
        let n = launch_identity(false, false);
        assert_eq!(
            (n.app_id, n.title, n.disguised),
            (APP_ID, "JizzCraft", false)
        );
    }

    #[test]
    fn desktop_entry_matches_the_disguised_app_id() {
        let text = include_str!("../../../assets/local.Viewer.desktop");
        let get = |k: &str| {
            text.lines()
                .find_map(|l| l.strip_prefix(&format!("{k}=")))
                .unwrap_or_else(|| panic!("missing {k}"))
        };
        assert_eq!(get("Name"), "Viewer");
        assert_eq!(get("Icon"), DISGUISE_APP_ID);
        assert_eq!(get("StartupWMClass"), DISGUISE_APP_ID);
        assert!(get("Exec").contains("--disguise"));
        // Only the command that starts the program may say what it is.
        let visible: String = text
            .lines()
            .filter(|l| !l.starts_with("Exec="))
            .collect::<Vec<_>>()
            .join("\n")
            .to_lowercase();
        assert!(!visible.contains("jizz"));
    }
}
