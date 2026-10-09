//! `EngineLocator` that finds engines the way UE's own tools do
//! (`FDesktopPlatformWindows::EnumerateEngineInstallations`), plus a directory
//! walk for native (in-tree) projects.
//!
//! An `EngineAssociation` is looked up, in order, in:
//! 1. the Epic Launcher's install list,
//!    `%ProgramData%\Epic\UnrealEngineLauncher\LauncherInstalled.dat` (entries
//!    named `UE_<version>`), which is what UE itself reads for Launcher engines;
//! 2. builds registered for this user, `HKCU\Software\Epic Games\Unreal Engine\Builds`
//!    (source builds, by GUID or any name);
//! 3. `HKLM\SOFTWARE\EpicGames\Unreal Engine\<version>`, which the Launcher also
//!    writes but which can be missing or stale.
//!
//! Nothing is assumed about where engines live: the first recorded folder that
//! actually contains an engine wins.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use tethys_core::AssociationKind;
use tethys_core::domain::classify_association;
use tethys_core::ports::{EngineLocator, PortResult};

#[derive(Debug, Default)]
pub struct RegistryEngineLocator;

impl EngineLocator for RegistryEngineLocator {
    fn locate(&self, association: &str, uproject: &Path) -> PortResult<PathBuf> {
        let association = association.trim();
        if classify_association(association) == AssociationKind::Native {
            return find_enclosing_engine(uproject).ok_or_else(|| {
                "EngineAssociation is empty and no parent directory contains \
                 Engine/Build/Build.version"
                    .into()
            });
        }
        let candidates = candidates(association);
        if let Some(found) = candidates.iter().find(|c| is_engine_root(&c.dir)) {
            return Ok(found.dir.clone());
        }
        Err(not_found(association, &candidates).into())
    }
}

/// A place an engine is recorded, and the folder that record names.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Candidate {
    source: &'static str,
    dir: PathBuf,
}

/// Every recorded location for `association`, in UE's order.
fn candidates(association: &str) -> Vec<Candidate> {
    let mut found = Vec::new();
    if let Some(dir) = launcher_list_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|text| launcher_install(&text, association))
    {
        found.push(Candidate {
            source: "the Epic Launcher's install list",
            dir,
        });
    }
    if let Some(dir) = registry::user_build(association) {
        found.push(Candidate {
            source: r"HKCU\Software\Epic Games\Unreal Engine\Builds",
            dir,
        });
    }
    if let Some(dir) = registry::machine_install(association) {
        found.push(Candidate {
            source: r"HKLM\SOFTWARE\EpicGames\Unreal Engine",
            dir,
        });
    }
    found
}

fn not_found(association: &str, candidates: &[Candidate]) -> String {
    let what = match classify_association(association) {
        AssociationKind::SourceBuild => format!("Source build {association}"),
        _ => format!("Unreal Engine {association}"),
    };
    if candidates.is_empty() {
        return format!(
            "{what} isn't installed on this machine: it's not in the Epic Launcher's \
             install list or the registry"
        );
    }
    let places: Vec<String> = candidates
        .iter()
        .map(|c| format!("{} (from {})", c.dir.display(), c.source))
        .collect();
    format!(
        "{what} is recorded at {}, but there's no engine there",
        places.join(" and ")
    )
}

/// Whether `dir` holds an engine, as UE checks it (`IsValidRootDirectory`).
fn is_engine_root(dir: &Path) -> bool {
    dir.join("Engine").join("Binaries").is_dir()
}

/// Walks up from the `.uproject` to the first directory containing
/// `Engine/Build/Build.version`.
pub fn find_enclosing_engine(uproject: &Path) -> Option<PathBuf> {
    uproject
        .ancestors()
        .skip(1)
        .find(|dir| {
            dir.join("Engine")
                .join("Build")
                .join("Build.version")
                .is_file()
        })
        .map(Path::to_path_buf)
}

/// `%ProgramData%\Epic\UnrealEngineLauncher\LauncherInstalled.dat`.
fn launcher_list_path() -> Option<PathBuf> {
    let data = std::env::var_os("ProgramData")?;
    Some(Path::new(&data).join(r"Epic\UnrealEngineLauncher\LauncherInstalled.dat"))
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct LauncherList {
    #[serde(default)]
    installation_list: Vec<LauncherEntry>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct LauncherEntry {
    app_name: String,
    install_location: String,
}

/// The folder of engine `version` (app `UE_<version>`) in the Launcher's
/// install list.
fn launcher_install(list: &str, version: &str) -> Option<PathBuf> {
    let list: LauncherList = serde_json::from_str(list).ok()?;
    let app = format!("UE_{version}");
    list.installation_list
        .into_iter()
        .find(|e| e.app_name == app)
        .map(|e| PathBuf::from(e.install_location))
}

#[cfg(windows)]
mod registry {
    use std::path::PathBuf;

    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

    /// A build registered for this user under any name, usually a GUID.
    pub fn user_build(name: &str) -> Option<PathBuf> {
        let builds = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(r"Software\Epic Games\Unreal Engine\Builds")
            .ok()?;
        builds
            .enum_values()
            .flatten()
            .find(|(value, _)| value.eq_ignore_ascii_case(name))
            .and_then(|(value, _)| builds.get_value::<String, _>(value).ok())
            .map(PathBuf::from)
    }

    /// A Launcher install recorded for the whole machine.
    pub fn machine_install(version: &str) -> Option<PathBuf> {
        RegKey::predef(HKEY_LOCAL_MACHINE)
            .open_subkey(format!(r"SOFTWARE\EpicGames\Unreal Engine\{version}"))
            .and_then(|k| k.get_value::<String, _>("InstalledDirectory"))
            .ok()
            .map(PathBuf::from)
    }
}

#[cfg(not(windows))]
mod registry {
    use std::path::PathBuf;

    pub fn user_build(_: &str) -> Option<PathBuf> {
        None
    }

    pub fn machine_install(_: &str) -> Option<PathBuf> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_enclosing_engine() {
        let root = std::env::temp_dir().join(format!("tethys-engine-{}", std::process::id()));
        std::fs::create_dir_all(root.join("Engine/Build")).unwrap();
        std::fs::write(root.join("Engine/Build/Build.version"), "{}").unwrap();
        let uproject = root.join("Games/Foo/Foo.uproject");

        assert_eq!(find_enclosing_engine(&uproject), Some(root.clone()));
        assert_eq!(
            RegistryEngineLocator.locate("", &uproject).unwrap(),
            root.clone()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reads_the_launcher_install_list() {
        let list = r#"{
            "InstallationList": [
                { "InstallLocation": "E:\\Epic\\Fortnite", "AppName": "Fortnite", "AppVersion": "x" },
                { "InstallLocation": "E:\\Engines\\UE_5.8", "NamespaceId": "ue", "AppName": "UE_5.8" },
                { "InstallLocation": "C:\\UE_5.7", "AppName": "UE_5.7" }
            ]
        }"#;
        assert_eq!(
            launcher_install(list, "5.8"),
            Some(PathBuf::from(r"E:\Engines\UE_5.8"))
        );
        assert_eq!(launcher_install(list, "5.6"), None);
        assert_eq!(launcher_install("not json", "5.8"), None);
    }

    #[test]
    fn explains_what_was_found() {
        let none = not_found("5.8", &[]);
        assert!(none.contains("Unreal Engine 5.8 isn't installed"), "{none}");
        let stale = not_found(
            "5.8",
            &[Candidate {
                source: "the Epic Launcher's install list",
                dir: PathBuf::from(r"Z:\gone"),
            }],
        );
        assert!(
            stale.contains(r"Z:\gone (from the Epic Launcher's install list)"),
            "{stale}"
        );
    }

    #[test]
    fn unknown_versions_are_not_found() {
        let err = RegistryEngineLocator
            .locate("0.0-tethys-test", Path::new(r"C:\x\x.uproject"))
            .unwrap_err();
        assert!(err.to_string().contains("isn't installed"), "{err}");
    }
}
