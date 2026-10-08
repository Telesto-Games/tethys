//! `SelfInstaller` that swaps the running exe for a downloaded one.
//!
//! Windows won't overwrite a running exe, so this uses `self-replace`, which
//! moves the old one out of the way and cleans it up after exit.

use std::path::{Path, PathBuf};

use tethys_core::ports::{PortResult, ProcessLauncher, SelfInstaller};
use tethys_core::unreal::CommandSpec;

use crate::process_launcher::DetachedLauncher;

#[derive(Debug)]
pub struct ExeReplacer {
    /// Where the exe was at startup. Captured up front because once it's
    /// replaced, `current_exe` points at the moved-away old one.
    exe: Option<PathBuf>,
}

impl ExeReplacer {
    pub fn new() -> Self {
        Self {
            exe: std::env::current_exe().ok(),
        }
    }

    fn exe(&self) -> PortResult<&Path> {
        self.exe
            .as_deref()
            .ok_or_else(|| "can't tell where the Tethys exe is".into())
    }
}

impl Default for ExeReplacer {
    fn default() -> Self {
        Self::new()
    }
}

impl SelfInstaller for ExeReplacer {
    fn staging_path(&self) -> PortResult<PathBuf> {
        Ok(self.exe()?.with_extension("exe.new"))
    }

    fn install(&self, new_exe: &Path) -> PortResult<()> {
        let result = self_replace::self_replace(new_exe);
        let _ = std::fs::remove_file(new_exe);
        result.map_err(|e| {
            let dir = self
                .exe
                .as_deref()
                .and_then(Path::parent)
                .unwrap_or(Path::new(""));
            format!("can't replace the Tethys exe in {}: {e}", dir.display()).into()
        })
    }

    fn relaunch(&self, args: &[String]) -> PortResult<()> {
        let exe = self.exe()?;
        let command = CommandSpec {
            program: exe.to_path_buf(),
            args: args.to_vec(),
        };
        DetachedLauncher.spawn_detached(&command, exe.parent().unwrap_or(Path::new(".")))
    }
}
