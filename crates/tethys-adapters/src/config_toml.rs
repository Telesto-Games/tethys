//! `ConfigStore` backed by TOML files in `%APPDATA%\tethys`.
//!
//! - `agents.toml`: `[[agent]]` profiles (user-edited, read-only to Tethys)
//! - `state.toml`: recent projects, build configuration, launch mode and a
//!   skipped release
//!   (written by Tethys)

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tethys_core::AgentProfile;
use tethys_core::ports::{ConfigStore, PortResult};
use tethys_core::theme::ThemeSpec;
use tethys_core::unreal::{self, BuildConfig, Configuration, LaunchMode};
use tethys_core::update::Version;

#[derive(Debug, Clone)]
pub struct TomlConfigStore {
    dir: PathBuf,
}

#[derive(Default, Deserialize)]
struct AgentsFile {
    #[serde(default)]
    agent: Vec<AgentProfile>,
}

#[derive(Default, Serialize, Deserialize)]
struct StateFile {
    #[serde(default)]
    recent_projects: Vec<PathBuf>,
    /// The editor configuration.
    #[serde(default)]
    build_configuration: Configuration,
    /// What Build builds; `None` until chosen, when it builds the editor in
    /// `build_configuration`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    build: Option<BuildConfig>,
    #[serde(default)]
    launch_mode: LaunchMode,
    /// Extra editor arguments; `None` until the user sets them (then the
    /// default applies), so an empty string means "none".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    editor_args: Option<String>,
    /// Extra arguments for the headless test editor; `None` until set, as above.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    test_editor_args: Option<String>,
    /// Tabs down the side of the main area rather than along the top.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    vertical_tabs: bool,
    /// A release the user chose to skip, e.g. "0.2.0".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    skipped_version: Option<String>,
    /// The theme in use, by name; `None` for the built-in one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    theme: Option<String>,
    /// Themes the user made.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    themes: Vec<ThemeSpec>,
}

impl TomlConfigStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// `%APPDATA%\tethys`, or `./.tethys` if `APPDATA` isn't set.
    pub fn user_default() -> Self {
        let base = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        Self::new(base.join("tethys"))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn read<T: Default + for<'de> Deserialize<'de>>(&self, name: &str) -> PortResult<T> {
        let path = self.dir.join(name);
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()).into())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
            Err(e) => Err(format!("{}: {e}", path.display()).into()),
        }
    }

    /// Reads `state.toml`, applies `change`, and writes it back atomically.
    fn update_state(&self, change: impl FnOnce(&mut StateFile)) -> PortResult<()> {
        let mut state = self.read::<StateFile>("state.toml")?;
        change(&mut state);
        std::fs::create_dir_all(&self.dir)?;
        let path = self.dir.join("state.toml");
        let tmp = self.dir.join("state.toml.tmp");
        std::fs::write(&tmp, toml::to_string(&state)?)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }
}

impl ConfigStore for TomlConfigStore {
    fn agent_profiles(&self) -> PortResult<Vec<AgentProfile>> {
        Ok(self.read::<AgentsFile>("agents.toml")?.agent)
    }

    fn recent_projects(&self) -> PortResult<Vec<PathBuf>> {
        Ok(self.read::<StateFile>("state.toml")?.recent_projects)
    }

    fn set_recent_projects(&self, projects: &[PathBuf]) -> PortResult<()> {
        self.update_state(|s| s.recent_projects = projects.to_vec())
    }

    fn build_configuration(&self) -> PortResult<Configuration> {
        Ok(self.read::<StateFile>("state.toml")?.build_configuration)
    }

    fn set_build_configuration(&self, configuration: Configuration) -> PortResult<()> {
        self.update_state(|s| s.build_configuration = configuration)
    }

    fn build_config(&self) -> PortResult<BuildConfig> {
        let state = self.read::<StateFile>("state.toml")?;
        Ok(state
            .build
            .unwrap_or(BuildConfig::editor(state.build_configuration)))
    }

    fn set_build_config(&self, config: BuildConfig) -> PortResult<()> {
        self.update_state(|s| s.build = Some(config))
    }

    fn launch_mode(&self) -> PortResult<LaunchMode> {
        Ok(self.read::<StateFile>("state.toml")?.launch_mode)
    }

    fn set_launch_mode(&self, mode: LaunchMode) -> PortResult<()> {
        self.update_state(|s| s.launch_mode = mode)
    }

    fn editor_args(&self) -> PortResult<String> {
        let state = self.read::<StateFile>("state.toml")?;
        Ok(state
            .editor_args
            .unwrap_or_else(|| unreal::DEFAULT_EDITOR_ARGS.to_string()))
    }

    fn set_editor_args(&self, args: &str) -> PortResult<()> {
        self.update_state(|s| s.editor_args = Some(args.trim().to_string()))
    }

    fn test_editor_args(&self) -> PortResult<String> {
        let state = self.read::<StateFile>("state.toml")?;
        Ok(state
            .test_editor_args
            .unwrap_or_else(|| unreal::DEFAULT_TEST_EDITOR_ARGS.to_string()))
    }

    fn set_test_editor_args(&self, args: &str) -> PortResult<()> {
        self.update_state(|s| s.test_editor_args = Some(args.trim().to_string()))
    }

    fn vertical_tabs(&self) -> PortResult<bool> {
        Ok(self.read::<StateFile>("state.toml")?.vertical_tabs)
    }

    fn set_vertical_tabs(&self, vertical: bool) -> PortResult<()> {
        self.update_state(|s| s.vertical_tabs = vertical)
    }

    fn skipped_version(&self) -> PortResult<Option<Version>> {
        let state = self.read::<StateFile>("state.toml")?;
        Ok(state.skipped_version.as_deref().and_then(Version::parse))
    }

    fn set_skipped_version(&self, version: Option<Version>) -> PortResult<()> {
        self.update_state(|s| s.skipped_version = version.map(|v| v.to_string()))
    }

    fn themes(&self) -> PortResult<Vec<ThemeSpec>> {
        Ok(self.read::<StateFile>("state.toml")?.themes)
    }

    fn active_theme(&self) -> PortResult<Option<String>> {
        Ok(self.read::<StateFile>("state.toml")?.theme)
    }

    fn set_themes(&self, themes: &[ThemeSpec], active: Option<&str>) -> PortResult<()> {
        self.update_state(|s| {
            s.themes = themes.to_vec();
            s.theme = active.map(str::to_string);
        })
    }
}

#[cfg(test)]
mod tests {
    use tethys_core::AdapterKind;

    use super::*;

    fn temp_store(name: &str) -> TomlConfigStore {
        let dir = std::env::temp_dir().join(format!("tethys-config-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        TomlConfigStore::new(dir)
    }

    #[test]
    fn missing_files_are_empty() {
        let store = temp_store("empty");
        assert!(store.agent_profiles().unwrap().is_empty());
        assert!(store.recent_projects().unwrap().is_empty());
    }

    #[test]
    fn reads_agents() {
        let store = temp_store("agents");
        std::fs::create_dir_all(store.dir()).unwrap();
        std::fs::write(
            store.dir().join("agents.toml"),
            r#"
            [[agent]]
            id = "claude"
            name = "Claude Code"
            adapter = "terminal"
            command = "claude"

            [[agent]]
            id = "opencode"
            name = "opencode"
            adapter = "acp"
            command = "opencode"
            args = ["acp"]
            "#,
        )
        .unwrap();
        let agents = store.agent_profiles().unwrap();
        assert_eq!(agents.len(), 2);
        assert_eq!(agents[1].adapter, AdapterKind::Acp);
        assert_eq!(agents[1].args, ["acp"]);
    }

    #[test]
    fn recent_projects_round_trip() {
        let store = temp_store("recent");
        let paths = vec![
            PathBuf::from(r"D:\a\A.uproject"),
            PathBuf::from(r"D:\b\B.uproject"),
        ];
        store.set_recent_projects(&paths).unwrap();
        assert_eq!(store.recent_projects().unwrap(), paths);
    }
}

#[cfg(test)]
mod state_tests {
    use super::*;

    #[test]
    fn build_configuration_round_trips_without_losing_recents() {
        let dir = std::env::temp_dir().join(format!("tethys-config-cfg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = TomlConfigStore::new(dir);
        assert_eq!(
            store.build_configuration().unwrap(),
            Configuration::Development
        );

        let recent = vec![PathBuf::from(r"D:\a\A.uproject")];
        store.set_recent_projects(&recent).unwrap();
        store
            .set_build_configuration(Configuration::DebugGame)
            .unwrap();
        assert_eq!(
            store.build_configuration().unwrap(),
            Configuration::DebugGame
        );
        assert_eq!(store.recent_projects().unwrap(), recent);
    }

    #[test]
    fn build_config_defaults_to_the_editor_configuration_then_round_trips() {
        let dir = std::env::temp_dir().join(format!("tethys-config-build-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = TomlConfigStore::new(dir);
        store
            .set_build_configuration(Configuration::DebugGame)
            .unwrap();
        assert_eq!(
            store.build_config().unwrap(),
            BuildConfig::editor(Configuration::DebugGame)
        );

        let shipping = BuildConfig {
            target: unreal::TargetKind::Game,
            configuration: Configuration::Shipping,
        };
        store.set_build_config(shipping).unwrap();
        store.set_launch_mode(LaunchMode::ServerAndClient).unwrap();
        store.set_editor_args("-log").unwrap();
        assert_eq!(store.build_config().unwrap(), shipping);
        assert_eq!(store.launch_mode().unwrap(), LaunchMode::ServerAndClient);
        assert_eq!(
            store.build_configuration().unwrap(),
            Configuration::DebugGame
        );
        assert_eq!(store.editor_args().unwrap(), "-log");
    }

    #[test]
    fn skipped_version_round_trips() {
        let dir = std::env::temp_dir().join(format!("tethys-config-skip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = TomlConfigStore::new(dir);
        assert_eq!(store.skipped_version().unwrap(), None);
        let v = Version::parse("0.2.0");
        store.set_skipped_version(v).unwrap();
        assert_eq!(store.skipped_version().unwrap(), v);
        store.set_skipped_version(None).unwrap();
        assert_eq!(store.skipped_version().unwrap(), None);
    }

    #[test]
    fn editor_args_default_until_set_even_to_nothing() {
        let dir = std::env::temp_dir().join(format!("tethys-config-args-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = TomlConfigStore::new(dir);
        assert_eq!(store.editor_args().unwrap(), unreal::DEFAULT_EDITOR_ARGS);
        store.set_editor_args("  -log -NoSplash ").unwrap();
        assert_eq!(store.editor_args().unwrap(), "-log -NoSplash");
        // Cleared on purpose: no arguments, not the default.
        store.set_editor_args("").unwrap();
        assert_eq!(store.editor_args().unwrap(), "");
    }

    #[test]
    fn test_editor_args_default_until_set_and_apart_from_editor_args() {
        let dir =
            std::env::temp_dir().join(format!("tethys-config-test-args-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = TomlConfigStore::new(dir);
        assert_eq!(
            store.test_editor_args().unwrap(),
            unreal::DEFAULT_TEST_EDITOR_ARGS
        );
        store
            .set_test_editor_args(" -nullrhi -DisablePlugins=FSR ")
            .unwrap();
        assert_eq!(
            store.test_editor_args().unwrap(),
            "-nullrhi -DisablePlugins=FSR"
        );
        assert_eq!(store.editor_args().unwrap(), unreal::DEFAULT_EDITOR_ARGS);
    }

    #[test]
    fn themes_round_trip_beside_other_state() {
        let dir = std::env::temp_dir().join(format!("tethys-config-themes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = TomlConfigStore::new(dir);
        assert!(store.themes().unwrap().is_empty());
        assert_eq!(store.active_theme().unwrap(), None);

        store.set_vertical_tabs(true).unwrap();
        let mine = ThemeSpec {
            name: "Mine".into(),
            accent: "#00ff00".into(),
            font_size: 14.,
            ..ThemeSpec::builtin()
        };
        store
            .set_themes(std::slice::from_ref(&mine), Some("Mine"))
            .unwrap();
        assert_eq!(store.themes().unwrap(), vec![mine.clone()]);
        assert_eq!(store.active_theme().unwrap().as_deref(), Some("Mine"));
        assert!(store.vertical_tabs().unwrap());

        store.set_themes(std::slice::from_ref(&mine), None).unwrap();
        assert_eq!(store.active_theme().unwrap(), None);
        assert_eq!(store.themes().unwrap(), vec![mine]);
    }

    #[test]
    fn a_theme_missing_fields_reads_with_builtin_values() {
        let dir = std::env::temp_dir().join(format!(
            "tethys-config-theme-partial-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let store = TomlConfigStore::new(dir);
        std::fs::create_dir_all(store.dir()).unwrap();
        std::fs::write(
            store.dir().join("state.toml"),
            "theme = \"Mine\"\n\n[[themes]]\nname = \"Mine\"\nbackground = \"#ffffff\"\n",
        )
        .unwrap();
        let themes = store.themes().unwrap();
        assert_eq!(themes[0].background, "#ffffff");
        assert_eq!(themes[0].foreground, ThemeSpec::builtin().foreground);
    }

    #[test]
    fn vertical_tabs_off_until_turned_on() {
        let dir = std::env::temp_dir().join(format!("tethys-config-tabs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = TomlConfigStore::new(dir);
        assert!(!store.vertical_tabs().unwrap());
        store.set_vertical_tabs(true).unwrap();
        assert!(store.vertical_tabs().unwrap());
        store.set_vertical_tabs(false).unwrap();
        assert!(!store.vertical_tabs().unwrap());
    }
}
