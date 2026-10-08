//! Unreal tool command lines: building with UBT and launching the editor.
//!
//! Pure functions from a project and engine root to a command. Running them is
//! the app's job (a terminal session for builds, a detached process for the editor).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::domain::Project;

/// A program plus arguments, not yet run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ToolError {
    #[error("the engine for this project wasn't found: {0}")]
    NoEngine(String),
    #[error("{0} has no C++ modules, so there's nothing to build")]
    NothingToBuild(String),
}

/// Platform for editor builds. Fixed for now.
pub const PLATFORM: &str = "Win64";

/// UBT build configuration for the editor target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Configuration {
    #[default]
    Development,
    /// Engine in Development, project modules unoptimised for debugging.
    DebugGame,
}

impl Configuration {
    pub const ALL: [Configuration; 2] = [Configuration::Development, Configuration::DebugGame];

    /// The name UBT uses.
    pub fn as_str(self) -> &'static str {
        match self {
            Configuration::Development => "Development",
            Configuration::DebugGame => "DebugGame",
        }
    }

    /// The editor executable for this configuration, relative to the engine root.
    fn editor_exe(self) -> &'static str {
        match self {
            Configuration::Development => r"Engine\Binaries\Win64\UnrealEditor.exe",
            Configuration::DebugGame => r"Engine\Binaries\Win64\UnrealEditor-Win64-DebugGame.exe",
        }
    }
}

/// The editor target to build: `<Name>Editor` if the project has that target,
/// otherwise the first `*Editor` target in `Source`, otherwise `<Name>Editor`.
pub fn editor_target(project: &Project) -> String {
    let preferred = format!("{}Editor", project.name());
    if project.targets.contains(&preferred) {
        return preferred;
    }
    project
        .targets
        .iter()
        .find(|t| t.ends_with("Editor"))
        .cloned()
        .unwrap_or(preferred)
}

fn engine_root(project: &Project) -> Result<&Path, ToolError> {
    project
        .engine
        .as_deref()
        .map_err(|e| ToolError::NoEngine(e.clone()))
}

/// `Engine\Build\BatchFiles\Build.bat <Target> Win64 <Configuration> -Project=<uproject> -WaitMutex`
pub fn build_command(
    project: &Project,
    configuration: Configuration,
) -> Result<CommandSpec, ToolError> {
    let engine = engine_root(project)?;
    if project.modules.is_empty() {
        return Err(ToolError::NothingToBuild(project.name().to_string()));
    }
    Ok(CommandSpec {
        program: engine.join(r"Engine\Build\BatchFiles\Build.bat"),
        args: vec![
            editor_target(project),
            PLATFORM.into(),
            configuration.as_str().into(),
            format!("-Project={}", project.path.display()),
            "-WaitMutex".into(),
        ],
    })
}

/// `UnrealEditor.exe <uproject>`, or `UnrealEditor-Win64-DebugGame.exe` for DebugGame.
pub fn editor_command(
    project: &Project,
    configuration: Configuration,
) -> Result<CommandSpec, ToolError> {
    let engine = engine_root(project)?;
    Ok(CommandSpec {
        program: engine.join(configuration.editor_exe()),
        args: vec![project.path.display().to_string()],
    })
}

/// Target names (`Foo` for `Foo.Target.cs`) from file names in `Source`.
pub fn targets_from_file_names<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut targets: Vec<String> = names
        .into_iter()
        .filter_map(|n| n.strip_suffix(".Target.cs"))
        .map(str::to_string)
        .collect();
    targets.sort();
    targets
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(engine: Result<&str, &str>, modules: &[&str], targets: &[&str]) -> Project {
        let mut p = Project::parse(r"D:\dev\Arcade\Arcade.uproject", "{}").unwrap();
        p.engine = engine.map(PathBuf::from).map_err(str::to_string);
        p.modules = modules.iter().map(|m| m.to_string()).collect();
        p.targets = targets.iter().map(|t| t.to_string()).collect();
        p
    }

    #[test]
    fn build_command_for_editor_target() {
        let p = project(Ok(r"D:\UE_5.8"), &["Arcade"], &["Arcade", "ArcadeEditor"]);
        let cmd = build_command(&p, Configuration::Development).unwrap();
        assert_eq!(
            cmd.program,
            Path::new(r"D:\UE_5.8\Engine\Build\BatchFiles\Build.bat")
        );
        assert_eq!(
            cmd.args,
            [
                "ArcadeEditor",
                "Win64",
                "Development",
                r"-Project=D:\dev\Arcade\Arcade.uproject",
                "-WaitMutex"
            ]
        );
    }

    #[test]
    fn editor_target_falls_back() {
        let other = project(Ok("E"), &["A"], &["Game", "GameEditor"]);
        assert_eq!(editor_target(&other), "GameEditor");
        let none = project(Ok("E"), &["A"], &[]);
        assert_eq!(editor_target(&none), "ArcadeEditor");
    }

    #[test]
    fn build_needs_engine_and_modules() {
        assert!(matches!(
            build_command(
                &project(Err("missing"), &["A"], &[]),
                Configuration::Development
            ),
            Err(ToolError::NoEngine(_))
        ));
        assert!(matches!(
            build_command(&project(Ok("E"), &[], &[]), Configuration::Development),
            Err(ToolError::NothingToBuild(_))
        ));
    }

    #[test]
    fn editor_command_opens_uproject() {
        let p = project(Ok(r"D:\UE_5.8"), &[], &[]);
        let cmd = editor_command(&p, Configuration::Development).unwrap();
        assert_eq!(
            cmd.program,
            Path::new(r"D:\UE_5.8\Engine\Binaries\Win64\UnrealEditor.exe")
        );
        assert_eq!(cmd.args, [r"D:\dev\Arcade\Arcade.uproject"]);
    }

    #[test]
    fn debuggame_builds_and_launches_debuggame() {
        let p = project(Ok(r"D:\UE_5.8"), &["Arcade"], &[]);
        let build = build_command(&p, Configuration::DebugGame).unwrap();
        assert_eq!(build.args[2], "DebugGame");
        let editor = editor_command(&p, Configuration::DebugGame).unwrap();
        assert_eq!(
            editor.program,
            Path::new(r"D:\UE_5.8\Engine\Binaries\Win64\UnrealEditor-Win64-DebugGame.exe")
        );
    }

    #[test]
    fn finds_targets() {
        let names = [
            "ArcadeEditor.Target.cs",
            "Arcade",
            "Arcade.Target.cs",
            "x.cs",
        ];
        assert_eq!(targets_from_file_names(names), ["Arcade", "ArcadeEditor"]);
    }
}
