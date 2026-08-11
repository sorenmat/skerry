//! Persistent user configuration and session state.
//!
//! The config file lives at `~/.config/skerry/config.json` (or the
//! platform equivalent) and stores settings and the list of recently
//! open files for session restore. When that file does not exist, Skerry
//! reads the previous product config locations for upgrade compatibility.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ViewState;

const APP_CONFIG_DIR: &str = "skerry";
const NOVA_CONFIG_DIR: &str = "nova";
const ORIGINAL_CONFIG_DIR: &str = "the_editor";
const CONFIG_FILE: &str = "config.json";
const MAX_RECENT_FILES: usize = 20;

/// User configuration and session state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    /// Name of the active syntax-highlighting theme. `None` means use
    /// the built-in default.
    pub theme: Option<String>,
    /// Name of the active GUI chrome theme. `None` means use the built-in
    /// default (dark).
    pub ui_theme: Option<String>,
    /// Recently opened files, most recent first.
    #[serde(default)]
    pub recent_files: Vec<PathBuf>,
    /// Whether the project-tree sidebar was open on last exit.
    #[serde(default)]
    pub project_tree_open: Option<bool>,
    /// Whether auto-save is enabled.
    #[serde(default = "default_auto_save")]
    pub auto_save: bool,
    /// Milliseconds of idle time before auto-saving a dirty buffer.
    #[serde(default = "default_auto_save_delay_ms")]
    pub auto_save_delay_ms: u64,
    /// Whether to auto-save immediately when the window loses focus.
    #[serde(default = "default_auto_save_on_focus_change")]
    pub auto_save_on_focus_change: bool,
    /// Last GUI window inner width, in pixels.
    #[serde(default)]
    pub window_width: Option<u32>,
    /// Last GUI window inner height, in pixels.
    #[serde(default)]
    pub window_height: Option<u32>,
    /// Default indent mode for new documents.
    #[serde(default)]
    pub use_spaces: Option<bool>,
    /// Default tab width for new documents.
    #[serde(default)]
    pub tab_width: Option<usize>,
    /// Default soft-wrap toggle for new documents.
    #[serde(default)]
    pub soft_wrap: Option<bool>,
    /// Default scroll margin for new documents.
    #[serde(default)]
    pub scroll_margin_lines: Option<usize>,
    /// Whether the git gutter is enabled by default for new documents.
    #[serde(default = "default_git_gutter")]
    pub git_gutter: bool,
    /// Whether inline git blame is enabled by default for new documents.
    /// Off by default — blame shells out to git on every refresh and
    /// clutters narrow windows.
    #[serde(default)]
    pub git_blame: bool,
    /// Whether the GUI caret slides between lines instead of snapping.
    #[serde(default)]
    pub caret_animation: bool,
    /// External formatter commands per language ID.
    #[serde(default = "default_formatters")]
    pub formatters: HashMap<String, String>,
    /// Snippet templates keyed by trigger word. When the user types a
    /// trigger and presses Tab, it's replaced by the template body.
    /// `$0` marks the final cursor position. Lines use `\n` in JSON.
    /// Example: {"for": "for ${1:item} in ${2:collection} {\n    $0\n}"}
    #[serde(default)]
    pub snippets: HashMap<String, String>,
}

fn default_git_gutter() -> bool {
    true
}

fn default_auto_save() -> bool {
    true
}

fn default_auto_save_delay_ms() -> u64 {
    2000
}

fn default_auto_save_on_focus_change() -> bool {
    true
}

/// Built-in external formatter commands per language. Users can override
/// these or add new entries in config.json.
fn default_formatters() -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert("rust".to_string(), "rustfmt --emit stdout".to_string());
    m.insert("go".to_string(), "gofmt".to_string());
    m.insert("python".to_string(), "ruff format -".to_string());
    m
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: None,
            ui_theme: None,
            recent_files: Vec::new(),
            project_tree_open: None,
            auto_save: default_auto_save(),
            auto_save_delay_ms: default_auto_save_delay_ms(),
            auto_save_on_focus_change: default_auto_save_on_focus_change(),
            window_width: None,
            window_height: None,
            use_spaces: None,
            tab_width: None,
            soft_wrap: None,
            scroll_margin_lines: None,
            git_gutter: default_git_gutter(),
            git_blame: false,
            caret_animation: false,
            formatters: default_formatters(),
            snippets: HashMap::new(),
        }
    }
}

impl Config {
    /// Load the config from disk, returning a default config if the file
    /// does not exist or cannot be parsed.
    pub fn load() -> Self {
        let Some(base_dir) = dirs::config_dir() else {
            return Self::default();
        };
        let current = base_dir.join(APP_CONFIG_DIR).join(CONFIG_FILE);
        let nova = base_dir.join(NOVA_CONFIG_DIR).join(CONFIG_FILE);
        let original = base_dir.join(ORIGINAL_CONFIG_DIR).join(CONFIG_FILE);
        Self::load_from_paths(&current, &[&nova, &original])
    }

    /// Save the config to disk. Errors are ignored — a missing config
    /// file just means defaults on next start.
    pub fn save(&self) {
        if let Some(dir) = Self::config_dir() {
            let _ = fs::create_dir_all(&dir);
            let path = dir.join(CONFIG_FILE);
            if let Ok(json) = serde_json::to_string_pretty(self) {
                let _ = fs::write(&path, json);
            }
        }
    }

    /// Directory containing the config file, if it can be determined.
    pub fn config_dir() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join(APP_CONFIG_DIR))
    }

    /// Full path to the config file, if it can be determined.
    pub fn config_path() -> Option<PathBuf> {
        Self::config_dir().map(|d| d.join(CONFIG_FILE))
    }

    fn load_from_paths(current: &Path, legacy: &[&Path]) -> Self {
        let path = if current.exists() {
            current
        } else {
            let Some(legacy) = legacy.iter().find(|path| path.exists()) else {
                return Self::default();
            };
            legacy
        };

        fs::read_to_string(path)
            .ok()
            .and_then(|contents| serde_json::from_str(&contents).ok())
            .unwrap_or_default()
    }

    /// Add a file to the top of the recent-files list, deduplicating and
    /// trimming to the max length.
    pub fn touch_recent_file(&mut self, path: &Path) {
        self.recent_files.retain(|p| p != path);
        self.recent_files.insert(0, path.to_path_buf());
        if self.recent_files.len() > MAX_RECENT_FILES {
            self.recent_files.truncate(MAX_RECENT_FILES);
        }
    }

    /// Remove a file from the recent-files list.
    pub fn remove_recent_file(&mut self, path: &Path) {
        self.recent_files.retain(|p| p != path);
    }

    /// Apply persisted per-document defaults to a new document's view.
    pub fn apply_document_defaults(&self, view: &mut ViewState) {
        if let Some(use_spaces) = self.use_spaces {
            view.use_spaces = use_spaces;
        }
        if let Some(tab_width) = self.tab_width {
            view.tab_width = tab_width.clamp(1, 16);
        }
        if let Some(soft_wrap) = self.soft_wrap {
            view.soft_wrap = soft_wrap;
        }
        if let Some(scroll_margin_lines) = self.scroll_margin_lines {
            view.scroll_margin_lines = scroll_margin_lines;
        }
        view.git_gutter_enabled = self.git_gutter;
        view.git_blame_enabled = self.git_blame;
    }

    /// Capture the active document's view settings as defaults for future
    /// new documents.
    pub fn capture_document_defaults(&mut self, view: &ViewState) {
        self.use_spaces = Some(view.use_spaces);
        self.tab_width = Some(view.tab_width);
        self.soft_wrap = Some(view.soft_wrap);
        self.scroll_margin_lines = Some(view.scroll_margin_lines);
        self.git_gutter = view.git_gutter_enabled;
        self.git_blame = view.git_blame_enabled;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn recent_files_deduplicates_and_limits() {
        let mut cfg = Config::default();
        cfg.touch_recent_file(Path::new("/a"));
        cfg.touch_recent_file(Path::new("/b"));
        cfg.touch_recent_file(Path::new("/a"));
        assert_eq!(
            cfg.recent_files,
            vec![PathBuf::from("/a"), PathBuf::from("/b")]
        );

        for i in 0..MAX_RECENT_FILES + 5 {
            cfg.touch_recent_file(Path::new(&format!("/file{i}")));
        }
        assert_eq!(cfg.recent_files.len(), MAX_RECENT_FILES);
        assert_eq!(
            cfg.recent_files[0],
            PathBuf::from(format!("/file{}", MAX_RECENT_FILES + 4))
        );
    }

    #[test]
    fn save_and_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let original = Config {
            theme: Some("base16-ocean.dark".to_string()),
            recent_files: vec![PathBuf::from("/tmp/foo.rs")],
            project_tree_open: Some(true),
            window_width: Some(1024),
            window_height: Some(768),
            use_spaces: Some(false),
            tab_width: Some(8),
            soft_wrap: Some(true),
            scroll_margin_lines: Some(5),
            git_gutter: false,
            caret_animation: true,
            ..Config::default()
        };
        let path = dir.path().join("config.json");
        fs::write(&path, serde_json::to_string_pretty(&original).unwrap()).unwrap();

        let loaded: Config = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(original, loaded);
    }

    #[test]
    fn load_falls_back_to_legacy_config_when_current_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        let current = dir.path().join("skerry/config.json");
        let nova = dir.path().join("nova/config.json");
        let original = dir.path().join("the_editor/config.json");
        fs::create_dir_all(nova.parent().unwrap()).unwrap();
        fs::write(&nova, r#"{"theme":"nova-theme"}"#).unwrap();
        fs::create_dir_all(original.parent().unwrap()).unwrap();
        fs::write(&original, r#"{"theme":"original-theme"}"#).unwrap();

        let loaded = Config::load_from_paths(&current, &[&nova, &original]);

        assert_eq!(loaded.theme.as_deref(), Some("nova-theme"));
    }

    #[test]
    fn current_config_takes_precedence_over_legacy_config() {
        let dir = tempfile::tempdir().unwrap();
        let current = dir.path().join("skerry/config.json");
        let legacy = dir.path().join("nova/config.json");
        fs::create_dir_all(current.parent().unwrap()).unwrap();
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        fs::write(&current, r#"{"theme":"skerry-theme"}"#).unwrap();
        fs::write(&legacy, r#"{"theme":"nova-theme"}"#).unwrap();

        let loaded = Config::load_from_paths(&current, &[&legacy]);

        assert_eq!(loaded.theme.as_deref(), Some("skerry-theme"));
    }

    #[test]
    fn original_config_is_used_when_nova_config_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        let current = dir.path().join("skerry/config.json");
        let nova = dir.path().join("nova/config.json");
        let original = dir.path().join("the_editor/config.json");
        fs::create_dir_all(original.parent().unwrap()).unwrap();
        fs::write(&original, r#"{"theme":"original-theme"}"#).unwrap();

        let loaded = Config::load_from_paths(&current, &[&nova, &original]);

        assert_eq!(loaded.theme.as_deref(), Some("original-theme"));
    }

    #[test]
    fn missing_caret_animation_defaults_to_off() {
        let loaded: Config = serde_json::from_str("{}").unwrap();
        assert!(!loaded.caret_animation);
    }

    #[test]
    fn apply_document_defaults_overrides_view_state() {
        let config = Config {
            use_spaces: Some(false),
            tab_width: Some(8),
            soft_wrap: Some(true),
            scroll_margin_lines: Some(5),
            git_gutter: false,
            ..Config::default()
        };

        let mut view = ViewState::default();
        config.apply_document_defaults(&mut view);

        assert!(!view.use_spaces);
        assert_eq!(view.tab_width, 8);
        assert!(view.soft_wrap);
        assert_eq!(view.scroll_margin_lines, 5);
        assert!(!view.git_gutter_enabled);
    }

    #[test]
    fn apply_document_defaults_leaves_unset_fields_unchanged() {
        let config = Config::default();
        let mut view = ViewState::default();
        config.apply_document_defaults(&mut view);

        assert!(view.use_spaces);
        assert_eq!(view.tab_width, 4);
        assert!(!view.soft_wrap);
        assert_eq!(view.scroll_margin_lines, 3);
    }

    #[test]
    fn apply_document_defaults_clamps_tab_width() {
        let config = Config {
            tab_width: Some(100),
            ..Config::default()
        };
        let mut view = ViewState::default();
        config.apply_document_defaults(&mut view);
        assert_eq!(view.tab_width, 16);
    }

    #[test]
    fn capture_document_defaults_copies_view_state() {
        let mut config = Config::default();
        let view = ViewState {
            use_spaces: false,
            tab_width: 2,
            soft_wrap: true,
            scroll_margin_lines: 1,
            git_gutter_enabled: false,
            ..ViewState::default()
        };

        config.capture_document_defaults(&view);

        assert_eq!(config.use_spaces, Some(false));
        assert_eq!(config.tab_width, Some(2));
        assert_eq!(config.soft_wrap, Some(true));
        assert_eq!(config.scroll_margin_lines, Some(1));
        assert!(!config.git_gutter);
    }
}
