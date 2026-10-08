//! `ProcessLauncher` that starts a program detached from Tethys.

use std::path::Path;
use std::process::{Command, Stdio};

use tethys_core::ports::{PortResult, ProcessLauncher};
use tethys_core::unreal::CommandSpec;

#[derive(Debug, Default)]
pub struct DetachedLauncher;

impl ProcessLauncher for DetachedLauncher {
    fn spawn_detached(&self, command: &CommandSpec, cwd: &Path) -> PortResult<()> {
        if !command.program.is_file() {
            return Err(format!("{} doesn't exist", command.program.display()).into());
        }
        let mut cmd = Command::new(&command.program);
        cmd.args(&command.args)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            use windows_sys::Win32::System::Threading::{
                CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS,
            };
            cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
        }
        // Dropping the Child doesn't kill or wait for it.
        cmd.spawn()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn missing_program_is_an_error() {
        let spec = CommandSpec {
            program: PathBuf::from(r"Z:\nope\UnrealEditor.exe"),
            args: vec![],
        };
        let err = DetachedLauncher
            .spawn_detached(&spec, &std::env::temp_dir())
            .unwrap_err();
        assert!(err.to_string().contains("doesn't exist"));
    }
}
