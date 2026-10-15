//! Unreal tool command lines: building with UBT and launching the editor.
//!
//! Pure functions from a project and engine root to a command. Running them is
//! the app's job (a terminal session for builds, a detached process for the editor).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

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
    #[error("{project} has no {kind} target (a Source/*{kind}.Target.cs file)")]
    NoTarget { project: String, kind: &'static str },
    #[error(
        "{0} needs a source-built engine; this engine is an installed build, which doesn't \
         include it"
    )]
    NeedsSourceEngine(String),
}

/// Platform for editor builds. Fixed for now.
pub const PLATFORM: &str = "Win64";

/// UBT build configuration: how optimised the engine and the project are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum Configuration {
    /// Engine and project modules unoptimised. Needs a source-built engine.
    Debug,
    /// Engine in Development, project modules unoptimised for debugging.
    DebugGame,
    #[default]
    Development,
    /// Like Shipping, but with stats, the console and profiling hooks. Not for
    /// editors.
    Test,
    /// What players get: no checks, no console. Not for editors.
    Shipping,
}

impl Configuration {
    pub const ALL: [Configuration; 5] = [
        Configuration::Debug,
        Configuration::DebugGame,
        Configuration::Development,
        Configuration::Test,
        Configuration::Shipping,
    ];

    /// The name UBT uses.
    pub fn as_str(self) -> &'static str {
        match self {
            Configuration::Debug => "Debug",
            Configuration::DebugGame => "DebugGame",
            Configuration::Development => "Development",
            Configuration::Test => "Test",
            Configuration::Shipping => "Shipping",
        }
    }

    /// What UBT appends to binaries built in this configuration: nothing for
    /// Development, `-Win64-DebugGame` and so on otherwise.
    fn binary_suffix(self) -> String {
        match self {
            Configuration::Development => String::new(),
            other => format!("-{PLATFORM}-{}", other.as_str()),
        }
    }

    /// The editor executable for this configuration, relative to the engine root.
    fn editor_exe(self) -> String {
        format!(
            r"Engine\Binaries\Win64\UnrealEditor{}.exe",
            self.binary_suffix()
        )
    }

    /// The console build of the editor, for running headless.
    fn editor_cmd_exe(self) -> String {
        format!(
            r"Engine\Binaries\Win64\UnrealEditor{}-Cmd.exe",
            self.binary_suffix()
        )
    }
}

/// The kind of a UBT target, which its `*.Target.cs` name declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum TargetKind {
    #[default]
    Editor,
    Game,
    Client,
    Server,
}

impl TargetKind {
    pub const ALL: [TargetKind; 4] = [
        TargetKind::Editor,
        TargetKind::Game,
        TargetKind::Client,
        TargetKind::Server,
    ];

    /// The kind a target name declares by convention: `FooEditor`,
    /// `FooClient`, `FooServer`, otherwise a game.
    pub fn of(target: &str) -> TargetKind {
        [TargetKind::Editor, TargetKind::Client, TargetKind::Server]
            .into_iter()
            .find(|kind| target.ends_with(kind.suffix()))
            .unwrap_or(TargetKind::Game)
    }

    /// The suffix on target names of this kind, which Visual Studio also
    /// appends to configuration names.
    fn suffix(self) -> &'static str {
        match self {
            TargetKind::Editor => "Editor",
            TargetKind::Game => "",
            TargetKind::Client => "Client",
            TargetKind::Server => "Server",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            TargetKind::Editor => "Editor",
            TargetKind::Game => "Game",
            TargetKind::Client => "Client",
            TargetKind::Server => "Server",
        }
    }

    /// The configurations this kind builds in, most used first. Editors have
    /// no Test or Shipping.
    pub fn configurations(self) -> &'static [Configuration] {
        match self {
            TargetKind::Editor => &[
                Configuration::DebugGame,
                Configuration::Development,
                Configuration::Debug,
            ],
            _ => &[
                Configuration::DebugGame,
                Configuration::Development,
                Configuration::Debug,
                Configuration::Test,
                Configuration::Shipping,
            ],
        }
    }
}

/// What Build builds: a kind of target in a configuration, named the way
/// Visual Studio names solution configurations ("DebugGame Editor",
/// "Shipping", "Development Server").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct BuildConfig {
    pub target: TargetKind,
    pub configuration: Configuration,
}

impl BuildConfig {
    pub fn editor(configuration: Configuration) -> BuildConfig {
        BuildConfig {
            target: TargetKind::Editor,
            configuration,
        }
    }

    pub fn is_editor(self) -> bool {
        self.target == TargetKind::Editor
    }

    pub fn label(self) -> String {
        match self.target.suffix() {
            "" => self.configuration.as_str().to_string(),
            suffix => format!("{} {suffix}", self.configuration.as_str()),
        }
    }

    /// Whether only a source-built engine can build this. Installed engines
    /// (the Launcher's, or one made with BuildGraph) ship no Debug or Test
    /// engine binaries and can't build Client or Server targets.
    pub fn needs_source_engine(self) -> bool {
        matches!(
            self.configuration,
            Configuration::Debug | Configuration::Test
        ) || matches!(self.target, TargetKind::Client | TargetKind::Server)
    }
}

/// The build configurations this project offers, in menu order: each kind of
/// target it has (an editor always), in each configuration that kind builds in.
pub fn build_configs(project: &Project) -> Vec<BuildConfig> {
    TargetKind::ALL
        .into_iter()
        .filter(|&kind| kind == TargetKind::Editor || target_of(project, kind).is_some())
        .flat_map(|target| {
            target
                .configurations()
                .iter()
                .map(move |&configuration| BuildConfig {
                    target,
                    configuration,
                })
        })
        .collect()
}

/// The project's target of `kind`: `<Name><Suffix>` if it has that target,
/// otherwise the first target of that kind.
pub fn target_of(project: &Project, kind: TargetKind) -> Option<String> {
    let preferred = format!("{}{}", project.name(), kind.suffix());
    if project.targets.contains(&preferred) {
        return Some(preferred);
    }
    project
        .targets
        .iter()
        .find(|t| TargetKind::of(t) == kind)
        .cloned()
}

/// The editor target to build: as [`target_of`], falling back to
/// `<Name>Editor` when the project's targets weren't found.
pub fn editor_target(project: &Project) -> String {
    target_of(project, TargetKind::Editor).unwrap_or_else(|| format!("{}Editor", project.name()))
}

/// The target `config` builds for this project.
pub fn build_target(project: &Project, config: BuildConfig) -> Result<String, ToolError> {
    match config.target {
        TargetKind::Editor => Ok(editor_target(project)),
        kind => target_of(project, kind).ok_or_else(|| ToolError::NoTarget {
            project: project.name().to_string(),
            kind: kind.as_str(),
        }),
    }
}

fn engine_root(project: &Project) -> Result<&Path, ToolError> {
    project
        .engine
        .as_deref()
        .map_err(|e| ToolError::NoEngine(e.clone()))
}

/// Fails for what this project's engine can't build or run.
fn check_engine(project: &Project, config: BuildConfig) -> Result<(), ToolError> {
    if project.installed_engine && config.needs_source_engine() {
        return Err(ToolError::NeedsSourceEngine(config.label()));
    }
    Ok(())
}

/// Build what changed, or clean first and build everything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BuildAction {
    #[default]
    Build,
    /// `-Rebuild`: what `Rebuild.bat` adds to `Build.bat`.
    Rebuild,
}

impl BuildAction {
    pub fn as_str(self) -> &'static str {
        match self {
            BuildAction::Build => "Build",
            BuildAction::Rebuild => "Rebuild",
        }
    }
}

/// `Engine\Build\BatchFiles\Build.bat <Target> Win64 <Configuration> -Project=<uproject> -WaitMutex`,
/// plus `-Rebuild` for a rebuild.
pub fn build_command(
    project: &Project,
    config: BuildConfig,
    action: BuildAction,
) -> Result<CommandSpec, ToolError> {
    let engine = engine_root(project)?;
    if project.modules.is_empty() {
        return Err(ToolError::NothingToBuild(project.name().to_string()));
    }
    check_engine(project, config)?;
    let mut args = vec![
        build_target(project, config)?,
        PLATFORM.into(),
        config.configuration.as_str().into(),
        format!("-Project={}", project.path.display()),
        "-WaitMutex".into(),
    ];
    if action == BuildAction::Rebuild {
        args.push("-Rebuild".into());
    }
    Ok(CommandSpec {
        program: engine.join(r"Engine\Build\BatchFiles\Build.bat"),
        args,
    })
}

/// The receipt UBT writes when `config` links:
/// `Binaries\Win64\<Target>[-Win64-<Configuration>].target`. Its time is the
/// time of the last successful build.
pub fn receipt_path(project: &Project, config: BuildConfig) -> Result<PathBuf, ToolError> {
    let target = build_target(project, config)?;
    Ok(project.root().join(format!(
        r"Binaries\{PLATFORM}\{target}{}.target",
        config.configuration.binary_suffix()
    )))
}

/// Whether a file's change means a build is needed: C++ and the C# that
/// describes modules and targets, plus the project and plugin descriptors.
fn is_source(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "c" | "cc" | "cpp" | "cxx" | "h" | "hpp" | "inl" | "cs" | "uproject" | "uplugin"
    )
}

/// The most recently changed source file under `root`: the `.uproject`,
/// `Source/`, and each plugin's descriptor and `Source/`. Content, Binaries
/// and Intermediate aren't source. `None` when there is none.
pub fn newest_source(root: &Path) -> Option<(PathBuf, SystemTime)> {
    let mut newest: Option<(PathBuf, SystemTime)> = None;
    let mut note = |path: PathBuf| {
        if !is_source(&path) {
            return;
        }
        if let Ok(modified) = std::fs::metadata(&path).and_then(|m| m.modified())
            && newest.as_ref().is_none_or(|(_, t)| modified > *t)
        {
            newest = Some((path, modified));
        }
    };
    for entry in std::fs::read_dir(root).into_iter().flatten().flatten() {
        note(entry.path());
    }
    walk(&root.join("Source"), &mut note);
    for plugin in std::fs::read_dir(root.join("Plugins"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let dir = plugin.path();
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            note(entry.path());
        }
        walk(&dir.join("Source"), &mut note);
    }
    newest
}

fn walk(dir: &Path, note: &mut impl FnMut(PathBuf)) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            walk(&path, note);
        } else {
            note(path);
        }
    }
}

/// Whether a configuration's last build is older than the source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Freshness {
    /// Built after the last source change.
    Current,
    /// No receipt: never built, or cleaned.
    NeverBuilt,
    /// `changed` was saved after the last build.
    Stale { changed: PathBuf },
}

impl Freshness {
    pub fn is_current(&self) -> bool {
        *self == Freshness::Current
    }
}

/// When each configuration of a project was last built, against its newest
/// source: one scan serves every configuration. Taken by [`BuildState::scan`]
/// in the background, since it reads the whole `Source` tree.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildState {
    newest_source: Option<(PathBuf, SystemTime)>,
    /// A receipt's time, or `None` when it doesn't exist; configurations
    /// without a target aren't listed.
    receipts: HashMap<BuildConfig, Option<SystemTime>>,
}

impl BuildState {
    pub fn scan(project: &Project) -> BuildState {
        let receipts = build_configs(project)
            .into_iter()
            .filter_map(|config| {
                let receipt = receipt_path(project, config).ok()?;
                let built = std::fs::metadata(receipt).and_then(|m| m.modified()).ok();
                Some((config, built))
            })
            .collect();
        BuildState {
            newest_source: newest_source(project.root()),
            receipts,
        }
    }

    /// `None` for a configuration the project has no target for.
    pub fn freshness(&self, config: BuildConfig) -> Option<Freshness> {
        let Some(built) = *self.receipts.get(&config)? else {
            return Some(Freshness::NeverBuilt);
        };
        Some(match &self.newest_source {
            Some((changed, modified)) if *modified > built => Freshness::Stale {
                changed: changed.clone(),
            },
            _ => Freshness::Current,
        })
    }
}

/// Extra editor arguments when the user hasn't set any: start the Unreal MCP
/// server (the `ModelContextProtocol` plugin). The editor ignores the switch
/// when the plugin isn't enabled.
pub const DEFAULT_EDITOR_ARGS: &str = "-ModelContextProtocolStartServer";

/// `UnrealEditor.exe <uproject> <extra_args…>`, or
/// `UnrealEditor-Win64-DebugGame.exe` and so on in other configurations.
pub fn editor_command(
    project: &Project,
    configuration: Configuration,
    extra_args: &[String],
) -> Result<CommandSpec, ToolError> {
    editor_exe_command(project, configuration, &[], extra_args)
}

/// The editor executable on the project with `mode_args` (a map or URL
/// first, then switches) before the user's `extra_args`.
fn editor_exe_command(
    project: &Project,
    configuration: Configuration,
    mode_args: &[&str],
    extra_args: &[String],
) -> Result<CommandSpec, ToolError> {
    let engine = engine_root(project)?;
    check_engine(project, BuildConfig::editor(configuration))?;
    let mut args = vec![project.path.display().to_string()];
    args.extend(mode_args.iter().map(|a| a.to_string()));
    args.extend(extra_args.iter().cloned());
    Ok(CommandSpec {
        program: engine.join(configuration.editor_exe()),
        args,
    })
}

/// What Launch starts. Every mode runs the editor executable, so none needs
/// cooked content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum LaunchMode {
    #[default]
    Editor,
    /// The game in its own window (`-game`), like Play → Standalone Game.
    Game,
    /// A dedicated server (`-server`), its log in a console window.
    Server,
    /// A dedicated server, then a game client that connects to it.
    ServerAndClient,
}

impl LaunchMode {
    pub const ALL: [LaunchMode; 4] = [
        LaunchMode::Editor,
        LaunchMode::Game,
        LaunchMode::Server,
        LaunchMode::ServerAndClient,
    ];

    /// The mode's name in a menu.
    pub fn as_str(self) -> &'static str {
        match self {
            LaunchMode::Editor => "Editor",
            LaunchMode::Game => "Standalone game",
            LaunchMode::Server => "Dedicated server",
            LaunchMode::ServerAndClient => "Server + client",
        }
    }

    /// The Launch button's label in this mode.
    pub fn action_label(self) -> &'static str {
        match self {
            LaunchMode::Editor => "Launch editor",
            LaunchMode::Game => "Launch game",
            LaunchMode::Server => "Launch server",
            LaunchMode::ServerAndClient => "Launch server + client",
        }
    }
}

/// Runs the editor executable as a game in a window.
const GAME_ARGS: [&str; 2] = ["-game", "-windowed"];
/// Runs the editor executable as a dedicated server. `-log` gives it a
/// console window: the only sign it's running, and the way to stop it.
const SERVER_ARGS: [&str; 2] = ["-server", "-log"];
/// The URL a client launched with a server connects to: that server, on the
/// default port.
const LOCAL_SERVER: &str = "127.0.0.1";

/// The processes Launch starts for `mode`, in order: the editor, or the
/// editor executable as a game, a server, or a server and then a client.
pub fn launch_commands(
    project: &Project,
    configuration: Configuration,
    mode: LaunchMode,
    extra_args: &[String],
) -> Result<Vec<CommandSpec>, ToolError> {
    let run =
        |mode_args: &[&str]| editor_exe_command(project, configuration, mode_args, extra_args);
    match mode {
        LaunchMode::Editor => Ok(vec![run(&[])?]),
        LaunchMode::Game => Ok(vec![run(&GAME_ARGS)?]),
        LaunchMode::Server => Ok(vec![run(&SERVER_ARGS)?]),
        LaunchMode::ServerAndClient => {
            let client = [LOCAL_SERVER, GAME_ARGS[0], GAME_ARGS[1]];
            Ok(vec![run(&SERVER_ARGS)?, run(&client)?])
        }
    }
}

/// Extra test editor arguments when the user hasn't set any: no rendering, so
/// it starts quickly and opens no window. Tests that render need it removed.
pub const DEFAULT_TEST_EDITOR_ARGS: &str = "-nullrhi";

/// A headless editor for running tests: `UnrealEditor-Cmd.exe <uproject>
/// -unattended -nosplash -nopause <extra_args…>`, which stays up until stopped.
pub fn test_editor_command(
    project: &Project,
    configuration: Configuration,
    extra_args: &[String],
) -> Result<CommandSpec, ToolError> {
    let engine = engine_root(project)?;
    check_engine(project, BuildConfig::editor(configuration))?;
    let mut args = vec![
        project.path.display().to_string(),
        "-unattended".into(),
        "-nosplash".into(),
        "-nopause".into(),
    ];
    args.extend(extra_args.iter().cloned());
    Ok(CommandSpec {
        program: engine.join(configuration.editor_cmd_exe()),
        args,
    })
}

/// Splits arguments typed on one line: separated by whitespace, with double
/// quotes grouping an argument that contains spaces (the quotes are dropped;
/// launching quotes again as needed).
pub fn split_args(line: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_arg = false;
    let mut quoted = false;
    for c in line.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                in_arg = true;
            }
            c if c.is_whitespace() && !quoted => {
                if in_arg {
                    args.push(std::mem::take(&mut current));
                    in_arg = false;
                }
            }
            c => {
                current.push(c);
                in_arg = true;
            }
        }
    }
    if in_arg {
        args.push(current);
    }
    args
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
        let cmd = build_command(
            &p,
            BuildConfig::editor(Configuration::Development),
            BuildAction::Build,
        )
        .unwrap();
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
                BuildConfig::default(),
                BuildAction::Build
            ),
            Err(ToolError::NoEngine(_))
        ));
        assert!(matches!(
            build_command(
                &project(Ok("E"), &[], &[]),
                BuildConfig::default(),
                BuildAction::Build
            ),
            Err(ToolError::NothingToBuild(_))
        ));
    }

    #[test]
    fn rebuild_adds_the_flag_rebuild_bat_adds() {
        let p = project(Ok(r"D:\UE_5.8"), &["Arcade"], &["Arcade", "ArcadeEditor"]);
        let config = BuildConfig {
            target: TargetKind::Game,
            configuration: Configuration::Shipping,
        };
        let cmd = build_command(&p, config, BuildAction::Rebuild).unwrap();
        assert_eq!(cmd.args[0], "Arcade");
        assert_eq!(cmd.args.last().unwrap(), "-Rebuild");
    }

    #[test]
    fn receipt_is_named_like_the_binaries() {
        let p = project(Ok("E"), &["Arcade"], &["Arcade", "ArcadeEditor"]);
        assert_eq!(
            receipt_path(&p, BuildConfig::editor(Configuration::Development)).unwrap(),
            Path::new(r"D:\dev\Arcade\Binaries\Win64\ArcadeEditor.target")
        );
        let debug_game = BuildConfig {
            target: TargetKind::Game,
            configuration: Configuration::DebugGame,
        };
        assert_eq!(
            receipt_path(&p, debug_game).unwrap(),
            Path::new(r"D:\dev\Arcade\Binaries\Win64\Arcade-Win64-DebugGame.target")
        );
        let server = BuildConfig {
            target: TargetKind::Server,
            configuration: Configuration::Development,
        };
        assert!(matches!(
            receipt_path(&p, server),
            Err(ToolError::NoTarget { .. })
        ));
    }

    /// A project on disk: `Arcade.uproject`, one module, a plugin, and the
    /// receipts asked for.
    fn disk_project(name: &str, receipts: &[&str]) -> Project {
        let root = std::env::temp_dir().join(format!("tethys-fresh-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for dir in [
            "Source/Arcade",
            "Plugins/Gizmo/Source/Gizmo",
            "Binaries/Win64",
            "Content",
        ] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        let json = r#"{"Modules":[{"Name":"Arcade"}]}"#;
        let uproject = root.join("Arcade.uproject");
        std::fs::write(&uproject, json).unwrap();
        std::fs::write(root.join("Source/Arcade.Target.cs"), "").unwrap();
        std::fs::write(root.join("Source/ArcadeEditor.Target.cs"), "").unwrap();
        std::fs::write(root.join("Source/Arcade/Arcade.cpp"), "").unwrap();
        std::fs::write(root.join("Plugins/Gizmo/Gizmo.uplugin"), "{}").unwrap();
        std::fs::write(root.join("Plugins/Gizmo/Source/Gizmo/Gizmo.h"), "").unwrap();
        std::fs::write(root.join("Content/Map.umap"), "").unwrap();
        for receipt in receipts {
            std::fs::write(root.join("Binaries/Win64").join(receipt), "{}").unwrap();
        }
        let mut p = Project::parse(&uproject, json).unwrap();
        p.engine = Ok(PathBuf::from("E"));
        p.targets = vec!["Arcade".into(), "ArcadeEditor".into()];
        p
    }

    /// Sets a file's modified time `secs` seconds after `base`, since writes
    /// within one test can land on the same timestamp.
    fn touch(path: &Path, base: SystemTime, secs: u64) {
        let time = base + std::time::Duration::from_secs(secs);
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(time)
            .unwrap();
    }

    #[test]
    fn newest_source_skips_content_and_sees_plugins() {
        let p = disk_project("newest", &[]);
        let root = p.root();
        let base = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        for file in [
            "Source/Arcade.Target.cs",
            "Source/ArcadeEditor.Target.cs",
            "Plugins/Gizmo/Gizmo.uplugin",
        ] {
            touch(&root.join(file), base, 0);
        }
        touch(&root.join("Arcade.uproject"), base, 1);
        touch(&root.join("Source/Arcade/Arcade.cpp"), base, 2);
        touch(&root.join("Plugins/Gizmo/Source/Gizmo/Gizmo.h"), base, 3);
        touch(&root.join("Content/Map.umap"), base, 9);
        let (path, _) = newest_source(root).unwrap();
        assert_eq!(path, root.join("Plugins/Gizmo/Source/Gizmo/Gizmo.h"));
        assert_eq!(newest_source(&root.join("nowhere")), None);
    }

    #[test]
    fn freshness_compares_the_receipt_with_the_newest_source() {
        let p = disk_project(
            "fresh",
            &["ArcadeEditor.target", "Arcade-Win64-DebugGame.target"],
        );
        let root = p.root();
        let base = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        touch(&root.join("Source/Arcade/Arcade.cpp"), base, 5);
        touch(&root.join("Binaries/Win64/ArcadeEditor.target"), base, 9);
        touch(
            &root.join("Binaries/Win64/Arcade-Win64-DebugGame.target"),
            base,
            1,
        );
        // Everything else was written just now, after the receipts.
        for file in [
            "Arcade.uproject",
            "Source/Arcade.Target.cs",
            "Source/ArcadeEditor.Target.cs",
            "Plugins/Gizmo/Gizmo.uplugin",
            "Plugins/Gizmo/Source/Gizmo/Gizmo.h",
        ] {
            touch(&root.join(file), base, 0);
        }
        let state = BuildState::scan(&p);

        let editor = BuildConfig::editor(Configuration::Development);
        assert_eq!(state.freshness(editor), Some(Freshness::Current));
        let game_debug = BuildConfig {
            target: TargetKind::Game,
            configuration: Configuration::DebugGame,
        };
        assert_eq!(
            state.freshness(game_debug),
            Some(Freshness::Stale {
                changed: root.join("Source/Arcade/Arcade.cpp")
            })
        );
        assert_eq!(
            state.freshness(BuildConfig::editor(Configuration::DebugGame)),
            Some(Freshness::NeverBuilt)
        );
        let server = BuildConfig {
            target: TargetKind::Server,
            configuration: Configuration::Development,
        };
        assert_eq!(state.freshness(server), None);
        assert_eq!(BuildState::default().freshness(editor), None);
    }

    #[test]
    fn editor_command_opens_uproject() {
        let p = project(Ok(r"D:\UE_5.8"), &[], &[]);
        let extra = vec!["-ModelContextProtocolStartServer".to_string()];
        let cmd = editor_command(&p, Configuration::Development, &extra).unwrap();
        assert_eq!(
            cmd.program,
            Path::new(r"D:\UE_5.8\Engine\Binaries\Win64\UnrealEditor.exe")
        );
        assert_eq!(
            cmd.args,
            [
                r"D:\dev\Arcade\Arcade.uproject",
                "-ModelContextProtocolStartServer"
            ]
        );
    }

    #[test]
    fn test_editor_command_runs_the_console_editor_unattended() {
        let p = project(Ok(r"D:\UE_5.8"), &[], &[]);
        let extra = vec!["-nullrhi".to_string()];
        let cmd = test_editor_command(&p, Configuration::DebugGame, &extra).unwrap();
        assert_eq!(
            cmd.program,
            Path::new(r"D:\UE_5.8\Engine\Binaries\Win64\UnrealEditor-Win64-DebugGame-Cmd.exe")
        );
        assert_eq!(
            cmd.args,
            [
                r"D:\dev\Arcade\Arcade.uproject",
                "-unattended",
                "-nosplash",
                "-nopause",
                "-nullrhi"
            ]
        );
    }

    #[test]
    fn splits_argument_lines() {
        assert_eq!(
            split_args(r#"  -log -ExecCmds="stat fps" "-Path=C:\My Dir" -x=""  "#),
            ["-log", "-ExecCmds=stat fps", r"-Path=C:\My Dir", "-x="]
        );
        assert!(split_args("   ").is_empty());
        assert_eq!(split_args(DEFAULT_EDITOR_ARGS), [DEFAULT_EDITOR_ARGS]);
    }

    #[test]
    fn debuggame_builds_and_launches_debuggame() {
        let p = project(Ok(r"D:\UE_5.8"), &["Arcade"], &[]);
        let build = build_command(
            &p,
            BuildConfig::editor(Configuration::DebugGame),
            BuildAction::Build,
        )
        .unwrap();
        assert_eq!(build.args[2], "DebugGame");
        let editor = editor_command(&p, Configuration::DebugGame, &[]).unwrap();
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

    fn config(target: TargetKind, configuration: Configuration) -> BuildConfig {
        BuildConfig {
            target,
            configuration,
        }
    }

    #[test]
    fn target_names_declare_their_kind() {
        assert_eq!(TargetKind::of("ArcadeEditor"), TargetKind::Editor);
        assert_eq!(TargetKind::of("ArcadeClient"), TargetKind::Client);
        assert_eq!(TargetKind::of("ArcadeServer"), TargetKind::Server);
        assert_eq!(TargetKind::of("Arcade"), TargetKind::Game);
    }

    #[test]
    fn configs_are_named_like_visual_studio() {
        let label = |t, c| config(t, c).label();
        assert_eq!(
            label(TargetKind::Editor, Configuration::DebugGame),
            "DebugGame Editor"
        );
        assert_eq!(label(TargetKind::Game, Configuration::Shipping), "Shipping");
        assert_eq!(
            label(TargetKind::Server, Configuration::Development),
            "Development Server"
        );
    }

    #[test]
    fn offers_each_target_the_project_has_in_its_configurations() {
        let p = project(Ok("E"), &["A"], &["Arcade", "ArcadeEditor", "ArcadeServer"]);
        let labels: Vec<String> = build_configs(&p)
            .into_iter()
            .map(BuildConfig::label)
            .collect();
        assert_eq!(
            labels,
            [
                "DebugGame Editor",
                "Development Editor",
                "Debug Editor",
                "DebugGame",
                "Development",
                "Debug",
                "Test",
                "Shipping",
                "DebugGame Server",
                "Development Server",
                "Debug Server",
                "Test Server",
                "Shipping Server",
            ]
        );
        // A project whose targets weren't found still offers its editor.
        let bare = project(Ok("E"), &["A"], &[]);
        assert!(build_configs(&bare).iter().all(|c| c.is_editor()));
    }

    #[test]
    fn builds_the_target_of_the_chosen_kind() {
        let p = project(Ok("E"), &["A"], &["Arcade", "ArcadeEditor", "Tools"]);
        let shipping = build_command(
            &p,
            config(TargetKind::Game, Configuration::Shipping),
            BuildAction::Build,
        );
        assert_eq!(shipping.unwrap().args[..3], ["Arcade", "Win64", "Shipping"]);
        assert_eq!(
            build_command(
                &p,
                config(TargetKind::Server, Configuration::Development),
                BuildAction::Build
            ),
            Err(ToolError::NoTarget {
                project: "Arcade".into(),
                kind: "Server"
            })
        );
        // Without `<Name>`, the first game target.
        let other = project(Ok("E"), &["A"], &["ArcadeEditor", "Tools"]);
        assert_eq!(
            target_of(&other, TargetKind::Game).as_deref(),
            Some("Tools")
        );
    }

    #[test]
    fn installed_engines_refuse_what_they_cant_build() {
        let mut p = project(Ok("E"), &["A"], &["Arcade", "ArcadeEditor", "ArcadeServer"]);
        p.installed_engine = true;
        let refused = |p: &Project, c| {
            matches!(
                build_command(p, c, BuildAction::Build),
                Err(ToolError::NeedsSourceEngine(_))
            )
        };
        assert!(refused(&p, BuildConfig::editor(Configuration::Debug)));
        assert!(refused(&p, config(TargetKind::Game, Configuration::Test)));
        assert!(refused(
            &p,
            config(TargetKind::Server, Configuration::Development)
        ));
        assert!(!refused(&p, BuildConfig::editor(Configuration::DebugGame)));
        assert!(!refused(
            &p,
            config(TargetKind::Game, Configuration::Shipping)
        ));
        assert!(matches!(
            editor_command(&p, Configuration::Debug, &[]),
            Err(ToolError::NeedsSourceEngine(_))
        ));
        p.installed_engine = false;
        assert!(!refused(&p, BuildConfig::editor(Configuration::Debug)));
    }

    #[test]
    fn debug_editor_is_its_own_executable() {
        let p = project(Ok(r"D:\UE"), &[], &[]);
        let editor = editor_command(&p, Configuration::Debug, &[]).unwrap();
        assert_eq!(
            editor.program,
            Path::new(r"D:\UE\Engine\Binaries\Win64\UnrealEditor-Win64-Debug.exe")
        );
        let test_editor = test_editor_command(&p, Configuration::Development, &[]).unwrap();
        assert_eq!(
            test_editor.program,
            Path::new(r"D:\UE\Engine\Binaries\Win64\UnrealEditor-Cmd.exe")
        );
    }

    #[test]
    fn launch_modes_run_the_editor_executable_as_a_game_or_server() {
        let p = project(Ok(r"D:\UE"), &[], &[]);
        let extra = vec!["-x".to_string()];
        let args = |mode| {
            launch_commands(&p, Configuration::Development, mode, &extra)
                .unwrap()
                .into_iter()
                .map(|cmd| {
                    assert_eq!(
                        cmd.program,
                        Path::new(r"D:\UE\Engine\Binaries\Win64\UnrealEditor.exe")
                    );
                    cmd.args[1..].join(" ")
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(args(LaunchMode::Editor), ["-x"]);
        assert_eq!(args(LaunchMode::Game), ["-game -windowed -x"]);
        assert_eq!(args(LaunchMode::Server), ["-server -log -x"]);
        assert_eq!(
            args(LaunchMode::ServerAndClient),
            ["-server -log -x", "127.0.0.1 -game -windowed -x"]
        );
    }
}
