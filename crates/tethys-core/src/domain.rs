//! Domain types: projects, agent profiles and sessions.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DomainError {
    #[error("agent `{id}` runs `claude`, which may only use the terminal adapter")]
    ClaudeOverAcp { id: String },
}

#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error("{path} is not a .uproject file")]
    NotUproject { path: PathBuf },
    #[error("can't read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path} is not a valid .uproject: {source}")]
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
}

/// What was opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectKind {
    /// A `.uproject`: builds, the editor and the debugger are available.
    Unreal,
    /// A plain folder: sessions, terminals and files only.
    Folder,
}

/// A parsed `.uproject` plus its resolved engine path, or a plain folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    /// Path to the `.uproject` file, or the folder itself.
    pub path: PathBuf,
    pub kind: ProjectKind,
    /// `EngineAssociation`: a version, a build GUID, or empty for a native project.
    pub engine_association: String,
    pub modules: Vec<String>,
    /// Plugins listed in the `.uproject`, with their enabled flag.
    pub plugins: Vec<(String, bool)>,
    /// UBT target names found in `Source/*.Target.cs`.
    pub targets: Vec<String>,
    /// Engine install root, or why it couldn't be resolved.
    pub engine: Result<PathBuf, String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct UprojectFile {
    #[serde(default)]
    engine_association: String,
    #[serde(default)]
    modules: Vec<UprojectModule>,
    #[serde(default)]
    plugins: Vec<UprojectPlugin>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct UprojectModule {
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct UprojectPlugin {
    name: String,
    #[serde(default)]
    enabled: bool,
}

impl Project {
    /// Parses `.uproject` JSON. The engine is left unresolved.
    pub fn parse(path: impl Into<PathBuf>, json: &str) -> Result<Project, ProjectError> {
        let path = path.into();
        let is_uproject = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("uproject"));
        if !is_uproject {
            return Err(ProjectError::NotUproject { path });
        }
        // Unreal writes a UTF-8 BOM on some files.
        let json = json.strip_prefix('\u{feff}').unwrap_or(json);
        let file: UprojectFile = match serde_json::from_str(json) {
            Ok(file) => file,
            Err(source) => return Err(ProjectError::Parse { path, source }),
        };
        Ok(Project {
            path,
            kind: ProjectKind::Unreal,
            engine_association: file.engine_association,
            modules: file.modules.into_iter().map(|m| m.name).collect(),
            plugins: file
                .plugins
                .into_iter()
                .map(|p| (p.name, p.enabled))
                .collect(),
            targets: Vec::new(),
            engine: Err("not resolved".into()),
        })
    }

    /// A plain folder, with nothing Unreal about it.
    pub fn folder(path: impl Into<PathBuf>) -> Project {
        Project {
            path: path.into(),
            kind: ProjectKind::Folder,
            engine_association: String::new(),
            modules: Vec::new(),
            plugins: Vec::new(),
            targets: Vec::new(),
            engine: Err("not an Unreal project".into()),
        }
    }

    pub fn is_unreal(&self) -> bool {
        self.kind == ProjectKind::Unreal
    }

    /// Project name: the `.uproject` file stem, or the folder name.
    pub fn name(&self) -> &str {
        let name = match self.kind {
            ProjectKind::Unreal => self.path.file_stem(),
            ProjectKind::Folder => self.path.file_name(),
        };
        // A drive root like `D:\` has no file name.
        name.or(Some(self.path.as_os_str()))
            .and_then(|s| s.to_str())
            .unwrap_or_default()
    }

    /// Directory containing the `.uproject`, or the folder itself; agent
    /// sessions run here.
    pub fn root(&self) -> &Path {
        match self.kind {
            ProjectKind::Unreal => self.path.parent().unwrap_or(Path::new(".")),
            ProjectKind::Folder => &self.path,
        }
    }

    /// What kind of engine `EngineAssociation` points at.
    pub fn association_kind(&self) -> AssociationKind {
        classify_association(&self.engine_association)
    }
}

/// The three forms of `EngineAssociation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssociationKind {
    /// Empty: the project lives inside a source engine tree.
    Native,
    /// A registered source build, keyed by GUID (`{...}`).
    SourceBuild,
    /// A Launcher install, keyed by version (`5.6`).
    Launcher,
}

pub fn classify_association(association: &str) -> AssociationKind {
    let a = association.trim();
    if a.is_empty() {
        AssociationKind::Native
    } else if a.starts_with('{') && a.ends_with('}') {
        AssociationKind::SourceBuild
    } else {
        AssociationKind::Launcher
    }
}

/// Which `AgentHost` adapter runs an agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AdapterKind {
    Terminal,
    Acp,
}

/// A named agent command line plus the adapter that runs it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentProfile {
    pub id: String,
    pub name: String,
    pub adapter: AdapterKind,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

impl AgentProfile {
    /// The built-in Claude Code profile.
    pub fn claude() -> AgentProfile {
        AgentProfile {
            id: "claude".into(),
            name: "Claude Code".into(),
            adapter: AdapterKind::Terminal,
            command: "claude".into(),
            args: vec![],
        }
    }

    /// The built-in opencode profile, running its own TUI.
    pub fn opencode() -> AgentProfile {
        AgentProfile {
            id: "opencode".into(),
            name: "opencode".into(),
            adapter: AdapterKind::Terminal,
            command: "opencode".into(),
            args: vec![],
        }
    }

    /// The profiles offered when no config exists. The first is the default.
    pub fn builtins() -> Vec<AgentProfile> {
        vec![AgentProfile::claude(), AgentProfile::opencode()]
    }

    /// Rejects profiles that would run Claude over ACP.
    ///
    /// A guard rail against mistakes, not a security boundary: a renamed
    /// binary defeats it.
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.adapter == AdapterKind::Acp && is_claude(&self.command) {
            return Err(DomainError::ClaudeOverAcp {
                id: self.id.clone(),
            });
        }
        Ok(())
    }
}

fn is_claude(command: &str) -> bool {
    let base = command.rsplit(['/', '\\']).next().unwrap_or(command);
    let stem = base.split('.').next().unwrap_or(base);
    stem.eq_ignore_ascii_case("claude")
}

/// Identifies one agent session for the lifetime of the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionId(pub u64);

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(adapter: AdapterKind, command: &str) -> AgentProfile {
        AgentProfile {
            id: "x".into(),
            name: "X".into(),
            adapter,
            command: command.into(),
            args: vec![],
        }
    }

    #[test]
    fn claude_over_acp_is_rejected() {
        for cmd in [
            "claude",
            "Claude.cmd",
            r"C:\npm\claude.exe",
            "/usr/bin/claude",
        ] {
            assert!(profile(AdapterKind::Acp, cmd).validate().is_err(), "{cmd}");
        }
    }

    #[test]
    fn claude_over_terminal_and_others_over_acp_are_allowed() {
        assert!(profile(AdapterKind::Terminal, "claude").validate().is_ok());
        assert!(profile(AdapterKind::Acp, "opencode").validate().is_ok());
    }

    const UPROJECT: &str = r#"{
        "FileVersion": 3,
        "EngineAssociation": "{8F2B1C3D-0000-0000-0000-000000000000}",
        "Category": "",
        "Modules": [
            { "Name": "ArcadeDemo", "Type": "Runtime", "LoadingPhase": "Default" }
        ],
        "Plugins": [
            { "Name": "ModelingToolsEditorMode", "Enabled": true },
            { "Name": "Bridge", "Enabled": false }
        ]
    }"#;

    #[test]
    fn parses_uproject() {
        let p = Project::parse(r"D:\dev\ArcadeDemo\ArcadeDemo.uproject", UPROJECT).unwrap();
        assert_eq!(p.name(), "ArcadeDemo");
        assert_eq!(p.root(), Path::new(r"D:\dev\ArcadeDemo"));
        assert_eq!(p.association_kind(), AssociationKind::SourceBuild);
        assert_eq!(p.modules, ["ArcadeDemo"]);
        assert_eq!(
            p.plugins,
            [
                ("ModelingToolsEditorMode".to_string(), true),
                ("Bridge".to_string(), false)
            ]
        );
    }

    #[test]
    fn parses_minimal_uproject_with_bom() {
        let p = Project::parse("Game.uproject", "\u{feff}{}").unwrap();
        assert_eq!(p.association_kind(), AssociationKind::Native);
        assert!(p.modules.is_empty());
    }

    #[test]
    fn rejects_bad_input() {
        assert!(matches!(
            Project::parse("Game.txt", "{}"),
            Err(ProjectError::NotUproject { .. })
        ));
        assert!(matches!(
            Project::parse("Game.uproject", "not json"),
            Err(ProjectError::Parse { .. })
        ));
    }

    #[test]
    fn folder_is_its_own_root() {
        let p = Project::folder(r"D:\dev\tools.v2");
        assert!(!p.is_unreal());
        assert_eq!(p.name(), "tools.v2");
        assert_eq!(p.root(), Path::new(r"D:\dev\tools.v2"));
        assert_eq!(Project::folder(r"D:\").name(), r"D:\");
        assert!(Project::parse("G.uproject", "{}").unwrap().is_unreal());
    }

    #[test]
    fn classifies_associations() {
        assert_eq!(classify_association(""), AssociationKind::Native);
        assert_eq!(classify_association("5.6"), AssociationKind::Launcher);
        assert_eq!(
            classify_association("{E1B6C1A4-1234-4C2B-9A7E-1234567890AB}"),
            AssociationKind::SourceBuild
        );
    }
}
