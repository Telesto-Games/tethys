//! `EngineLocator` backed by the Windows registry, plus a directory walk for
//! native (in-tree) projects.

use std::path::{Path, PathBuf};

use tethys_core::AssociationKind;
use tethys_core::domain::classify_association;
use tethys_core::ports::{EngineLocator, PortResult};

#[derive(Debug, Default)]
pub struct RegistryEngineLocator;

impl EngineLocator for RegistryEngineLocator {
    fn locate(&self, association: &str, uproject: &Path) -> PortResult<PathBuf> {
        let association = association.trim();
        let root = match classify_association(association) {
            AssociationKind::Native => find_enclosing_engine(uproject).ok_or(
                "EngineAssociation is empty and no parent directory contains Engine/Build/Build.version",
            )?,
            AssociationKind::Launcher => launcher_install(association)?,
            AssociationKind::SourceBuild => source_build(association)?,
        };
        if !root.join("Engine").is_dir() {
            return Err(format!("engine {} has no Engine directory", root.display()).into());
        }
        Ok(root)
    }
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

#[cfg(windows)]
fn launcher_install(version: &str) -> PortResult<PathBuf> {
    use winreg::RegKey;
    use winreg::enums::HKEY_LOCAL_MACHINE;

    let key = format!(r"SOFTWARE\EpicGames\Unreal Engine\{version}");
    let dir: String = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey(&key)
        .and_then(|k| k.get_value("InstalledDirectory"))
        .map_err(|_| format!(r"Unreal Engine {version} is not installed (no HKLM\{key})"))?;
    Ok(PathBuf::from(dir))
}

#[cfg(windows)]
fn source_build(guid: &str) -> PortResult<PathBuf> {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;

    const BUILDS: &str = r"Software\Epic Games\Unreal Engine\Builds";
    let builds = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(BUILDS)
        .map_err(|_| format!(r"no source builds are registered (no HKCU\{BUILDS})"))?;
    for (name, _) in builds.enum_values().flatten() {
        if name.eq_ignore_ascii_case(guid) {
            let dir: String = builds.get_value(&name)?;
            return Ok(PathBuf::from(dir));
        }
    }
    Err(format!(r"source build {guid} is not registered under HKCU\{BUILDS}").into())
}

#[cfg(not(windows))]
fn launcher_install(version: &str) -> PortResult<PathBuf> {
    Err(format!("can't look up Unreal Engine {version} on this platform").into())
}

#[cfg(not(windows))]
fn source_build(guid: &str) -> PortResult<PathBuf> {
    Err(format!("can't look up source build {guid} on this platform").into())
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
}
