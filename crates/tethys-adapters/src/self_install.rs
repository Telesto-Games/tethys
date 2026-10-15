//! `SelfInstaller` that swaps the running exe for a downloaded one.
//!
//! Windows won't overwrite a running exe, but it will rename one. So the
//! running exe is renamed to `<name>.old`, then the download, already next to
//! it, is renamed into its place. If that fails the old exe is renamed back,
//! so a failed update never leaves Tethys missing. The `.old` file can't be
//! deleted while its process runs; the relaunched Tethys removes it once the
//! old one has exited.

use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use tethys_core::ports::{PortResult, ProcessLauncher, SelfInstaller};
use tethys_core::unreal::CommandSpec;

use crate::process_launcher::DetachedLauncher;

/// How often a rename is tried before giving up. A freshly written exe is
/// often held open by an antivirus scan for a moment, which makes renaming it
/// fail with a sharing violation.
const RENAME_ATTEMPTS: u32 = 20;
const RENAME_RETRY_DELAY: Duration = Duration::from_millis(150);
/// How long the relaunched Tethys keeps trying to remove the `.old` exe: the
/// Tethys that updated starts it, then takes a few seconds to exit.
const OLD_REMOVAL_ATTEMPTS: u32 = 30;
const OLD_REMOVAL_DELAY: Duration = Duration::from_secs(1);

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
        let exe = self.exe()?;
        let rename = |from: &Path, to: &Path| {
            retrying(RENAME_ATTEMPTS, RENAME_RETRY_DELAY, || {
                std::fs::rename(from, to)
            })
        };
        swap(exe, new_exe, rename).map_err(|e| {
            let dir = exe.parent().unwrap_or(Path::new(""));
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

    fn remove_leftovers(&self) {
        let Ok(exe) = self.exe() else {
            return;
        };
        let exe = exe.to_path_buf();
        // Off the UI thread: %TEMP% can be large, and the `.old` exe is held
        // until the Tethys that updated to this one has exited.
        thread::spawn(move || {
            remove_leftovers(&exe, &std::env::temp_dir());
            let old = old_path(&exe);
            for _ in 0..OLD_REMOVAL_ATTEMPTS {
                if !old.exists() {
                    break;
                }
                thread::sleep(OLD_REMOVAL_DELAY);
                let _ = std::fs::remove_file(&old);
            }
        });
    }
}

/// Where the running exe is moved to while the new one takes its place.
fn old_path(exe: &Path) -> PathBuf {
    exe.with_extension("exe.old")
}

/// Puts `new_exe` in place of `exe`, moving `exe` to its `.old` path, with
/// `rename` doing each move. On failure `exe` is back where it was and
/// `new_exe` is kept.
fn swap(
    exe: &Path,
    new_exe: &Path,
    rename: impl Fn(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    // Checked first, so a missing download fails before anything moves.
    if !std::fs::metadata(new_exe)?.is_file() {
        return Err(io::Error::other(format!(
            "{} isn't a file",
            new_exe.display()
        )));
    }
    let old = old_path(exe);
    // Left by an earlier update. One that can't be removed is still running:
    // the Tethys that updated earlier hasn't exited yet.
    match std::fs::remove_file(&old) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => {
            return Err(io::Error::other(format!(
                "{} is still in use, probably by an earlier Tethys that hasn't exited ({e}). \
                 Close it and try again.",
                old.display()
            )));
        }
        _ => {}
    }
    rename(exe, &old)?;
    if let Err(e) = rename(new_exe, exe) {
        return match rename(&old, exe) {
            Ok(()) => Err(e),
            Err(back) => Err(io::Error::other(format!(
                "{e}; and putting the old exe back failed too ({back}), so it is now {}",
                old.display()
            ))),
        };
    }
    Ok(())
}

/// Runs `op` until it succeeds or has been tried `attempts` times.
fn retrying(attempts: u32, delay: Duration, op: impl Fn() -> io::Result<()>) -> io::Result<()> {
    let mut tries = 1;
    loop {
        match op() {
            Err(e) if tries < attempts && e.kind() != io::ErrorKind::NotFound => {
                tries += 1;
                thread::sleep(delay);
            }
            result => return result,
        }
    }
}

/// Removes what earlier updates left: the `.old` exe and partial downloads
/// next to `exe`, and the moved-away and helper copies the `self-replace`
/// crate (used before) made next to it or in `temp`, named
/// `.<stem>.<random>.__relocated__.exe` and so on. One that is still running
/// can't be removed and is left for next time.
fn remove_leftovers(exe: &Path, temp: &Path) {
    let _ = std::fs::remove_file(old_path(exe));
    let (Some(dir), Some(stem), Some(file)) = (
        exe.parent(),
        exe.file_stem().and_then(|s| s.to_str()),
        exe.file_name().and_then(|s| s.to_str()),
    ) else {
        return;
    };
    let replaced = format!(".{stem}.");
    let partial = format!("{file}.new.");
    let is_replaced = |name: &str| {
        name.starts_with(&replaced)
            && [".__relocated__.exe", ".__selfdelete__.exe", ".__temp__.exe"]
                .iter()
                .any(|suffix| name.ends_with(suffix))
    };
    let is_partial = |name: &str| name.starts_with(&partial) && name.ends_with(".part");
    remove_matching(dir, |name| is_replaced(name) || is_partial(name));
    if temp != dir {
        remove_matching(temp, is_replaced);
    }
}

/// Removes the files in `dir` whose names `matches` accepts, ignoring errors.
fn remove_matching(dir: &Path, matches: impl Fn(&str) -> bool) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_str().is_some_and(&matches) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::fs;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use super::{RENAME_ATTEMPTS, RENAME_RETRY_DELAY, old_path, remove_leftovers, retrying, swap};

    /// A fresh, empty directory for one test.
    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("tethys-self-install-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn rename(from: &Path, to: &Path) -> io::Result<()> {
        fs::rename(from, to)
    }

    #[test]
    fn the_new_exe_takes_the_old_ones_place() {
        let dir = scratch("swap");
        let exe = dir.join("tethys.exe");
        let new = dir.join("tethys.exe.new");
        fs::write(&exe, "old").unwrap();
        fs::write(&new, "new").unwrap();

        swap(&exe, &new, rename).unwrap();

        assert_eq!(fs::read_to_string(&exe).unwrap(), "new");
        assert_eq!(fs::read_to_string(old_path(&exe)).unwrap(), "old");
        assert!(!new.exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_missing_download_moves_nothing() {
        let dir = scratch("missing");
        let exe = dir.join("tethys.exe");
        fs::write(&exe, "old").unwrap();

        assert!(swap(&exe, &dir.join("tethys.exe.new"), rename).is_err());

        assert_eq!(fs::read_to_string(&exe).unwrap(), "old");
        assert!(!old_path(&exe).exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_failed_swap_puts_the_old_exe_back_and_keeps_the_download() {
        let dir = scratch("rollback");
        let exe = dir.join("tethys.exe");
        let new = dir.join("tethys.exe.new");
        fs::write(&exe, "old").unwrap();
        fs::write(&new, "new").unwrap();
        // Moving the download into place fails; every other move works.
        let failing = |from: &Path, to: &Path| {
            if from == new {
                Err(io::Error::other("sharing violation"))
            } else {
                fs::rename(from, to)
            }
        };

        let err = swap(&exe, &new, failing).unwrap_err();

        assert_eq!(err.to_string(), "sharing violation");
        assert_eq!(fs::read_to_string(&exe).unwrap(), "old");
        assert_eq!(fs::read_to_string(&new).unwrap(), "new");
        assert!(!old_path(&exe).exists());
        let _ = fs::remove_dir_all(dir);
    }

    /// The swap works on an exe that is running, as Tethys's own is.
    #[cfg(windows)]
    #[test]
    fn a_running_exe_is_swapped() {
        let dir = scratch("running");
        let exe = dir.join("tethys.exe");
        let new = dir.join("tethys.exe.new");
        let system = std::env::var_os("SystemRoot").map_or("C:\\Windows".into(), PathBuf::from);
        fs::copy(system.join("System32").join("ping.exe"), &exe).unwrap();
        fs::write(&new, "new").unwrap();
        let mut running = std::process::Command::new(&exe)
            .args(["-n", "30", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();

        let result = swap(&exe, &new, |from, to| {
            retrying(RENAME_ATTEMPTS, RENAME_RETRY_DELAY, || fs::rename(from, to))
        });
        let _ = running.kill();
        let _ = running.wait();

        result.unwrap();
        assert_eq!(fs::read_to_string(&exe).unwrap(), "new");
        assert!(old_path(&exe).exists());
        let _ = fs::remove_dir_all(dir);
    }

    /// An `.old` exe still in use means an earlier Tethys is still running
    /// from it: the swap stops before moving anything, and says so.
    #[cfg(windows)]
    #[test]
    fn an_old_exe_in_use_stops_the_swap() {
        use std::os::windows::fs::OpenOptionsExt;

        let dir = scratch("old-in-use");
        let exe = dir.join("tethys.exe");
        let new = dir.join("tethys.exe.new");
        fs::write(&exe, "old").unwrap();
        fs::write(&new, "new").unwrap();
        fs::write(old_path(&exe), "older").unwrap();
        // No sharing, as a running exe is held.
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(old_path(&exe))
            .unwrap();

        let err = swap(&exe, &new, rename).unwrap_err();
        drop(held);

        assert!(err.to_string().contains("earlier Tethys"), "{err}");
        assert_eq!(fs::read_to_string(&exe).unwrap(), "old");
        assert_eq!(fs::read_to_string(&new).unwrap(), "new");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_stale_old_exe_is_replaced() {
        let dir = scratch("stale");
        let exe = dir.join("tethys.exe");
        let new = dir.join("tethys.exe.new");
        fs::write(&exe, "old").unwrap();
        fs::write(&new, "new").unwrap();
        fs::write(old_path(&exe), "older").unwrap();

        swap(&exe, &new, rename).unwrap();

        assert_eq!(fs::read_to_string(old_path(&exe)).unwrap(), "old");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn retries_until_it_works_but_not_for_a_missing_file() {
        let calls = Cell::new(0);
        let flaky = || {
            calls.set(calls.get() + 1);
            match calls.get() {
                3 => Ok(()),
                _ => Err(io::Error::other("busy")),
            }
        };
        assert!(retrying(5, Duration::ZERO, flaky).is_ok());
        assert_eq!(calls.get(), 3);

        calls.set(0);
        let always = || {
            calls.set(calls.get() + 1);
            Err(io::Error::other("busy"))
        };
        assert!(retrying(4, Duration::ZERO, always).is_err());
        assert_eq!(calls.get(), 4);

        calls.set(0);
        let missing = || {
            calls.set(calls.get() + 1);
            Err(io::Error::from(io::ErrorKind::NotFound))
        };
        assert!(retrying(4, Duration::ZERO, missing).is_err());
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn leftovers_of_earlier_updates_are_removed() {
        let dir = scratch("leftovers");
        let temp = scratch("leftovers-temp");
        let exe = dir.join("tethys.exe");
        let keep = [
            "tethys.exe",
            "tethys.exe.new",
            "tethys.pdb",
            ".tethys.notes.txt",
            ".other.abc.__relocated__.exe",
        ];
        let remove = [
            "tethys.exe.old",
            "tethys.exe.new.1234.part",
            ".tethys.aausczggvluaaqufkmwxxfnmzyxqbhyd.__selfdelete__.exe",
            ".tethys.cswkecffzkeibtcksolgejfttytvavix.__relocated__.exe",
            ".tethys.qwertyuiopasdfghjklzxcvbnmqwerty.__temp__.exe",
        ];
        for name in keep.iter().chain(&remove) {
            fs::write(dir.join(name), "").unwrap();
        }
        let temp_keep = ["tethys.exe.new.1234.part", "other.tmp"];
        let temp_remove = [".tethys.zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz.__relocated__.exe"];
        for name in temp_keep.iter().chain(&temp_remove) {
            fs::write(temp.join(name), "").unwrap();
        }

        remove_leftovers(&exe, &temp);

        for name in keep {
            assert!(dir.join(name).exists(), "{name} kept");
        }
        for name in remove {
            assert!(!dir.join(name).exists(), "{name} removed");
        }
        for name in temp_keep {
            assert!(temp.join(name).exists(), "{name} kept in temp");
        }
        for name in temp_remove {
            assert!(!temp.join(name).exists(), "{name} removed from temp");
        }
        let _ = fs::remove_dir_all(dir);
        let _ = fs::remove_dir_all(temp);
    }
}
