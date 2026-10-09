//! `ConfigStore` backed by TOML files in `%APPDATA%\tethys`.
//!
//! - `agents.toml`: `[[agent]]` profiles (user-edited, read-only to Tethys)
//! - `state.toml`: recent projects, build configuration and a skipped release
//!   (written by Tethys)

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tethys_core::AgentProfile;
use tethys_core::ports::{ConfigStore, PortResult};
use tethys_core::unreal::{self, Configuration};
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
    #[serde(default)]
    build_configuration: Configuration,
    /// Extra editor arguments; `None` until the user sets them (then the
    /// default applies), so an empty string means "none".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    editor_args: Option<String>,
    /// A release the user chose to skip, e.g. "0.2.0".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    skipped_version: Option<String>,
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

    fn editor_args(&self) -> PortResult<String> {
        let state = self.read::<StateFile>("state.toml")?;
        Ok(state
            .editor_args
            .unwrap_or_else(|| unreal::DEFAULT_EDITOR_ARGS.to_string()))
    }

    fn set_editor_args(&self, args: &str) -> PortResult<()> {
        self.update_state(|s| s.editor_args = Some(args.trim().to_string()))
    }

    fn skipped_version(&self) -> PortResult<Option<Version>> {
        let state = self.read::<StateFile>("state.toml")?;
        Ok(state.skipped_version.as_deref().and_then(Version::parse))
    }

    fn set_skipped_version(&self, version: Option<Version>) -> PortResult<()> {
        self.update_state(|s| s.skipped_version = version.map(|v| v.to_string()))
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
}
