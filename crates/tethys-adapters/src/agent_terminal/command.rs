//! Turns an agent command (`claude`) into something CreateProcess can run.
//!
//! CreateProcess only finds `.exe` files, but npm installs agents as `.cmd`
//! shims. We search PATH with PATHEXT ourselves and run batch files through
//! `cmd.exe`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// A resolved command line, ready for the PTY.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandLine {
    /// Program, already quoted if needed.
    pub program: String,
    pub args: Vec<String>,
    /// Whether the PTY should quote `args` (false when we've built a `cmd /s /c` line).
    pub escape_args: bool,
}

pub fn resolve(command: &str, args: &[String]) -> Result<CommandLine, String> {
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
    let exe = find_executable(command, &path_var, &pathext)
        .ok_or_else(|| format!("`{command}` was not found on PATH"))?;
    Ok(command_line(&exe, args))
}

fn command_line(exe: &Path, args: &[String]) -> CommandLine {
    let is_batch = exe
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"));
    if !is_batch {
        return CommandLine {
            program: quote(&exe.to_string_lossy()),
            args: args.to_vec(),
            escape_args: true,
        };
    }
    // `cmd /s /c ""C:\x\claude.cmd" arg"`: with /s, cmd strips exactly the
    // outer quotes and runs the rest verbatim.
    let comspec = std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".into());
    let mut inner = quote(&exe.to_string_lossy());
    for arg in args {
        inner.push(' ');
        inner.push_str(&quote(arg));
    }
    CommandLine {
        program: quote(&comspec),
        args: vec![
            "/d".into(),
            "/s".into(),
            "/c".into(),
            format!("\"{inner}\""),
        ],
        escape_args: false,
    }
}

fn quote(s: &str) -> String {
    if s.is_empty() || s.contains([' ', '\t', '"']) {
        format!("\"{}\"", s.replace('"', "\\\""))
    } else {
        s.to_string()
    }
}

/// Finds `command` like cmd.exe would: as a path if it has a directory
/// component, otherwise on PATH, trying each PATHEXT extension.
pub fn find_executable(command: &str, path_var: &OsString, pathext: &str) -> Option<PathBuf> {
    let candidates = |base: PathBuf| -> Option<PathBuf> {
        if base.extension().is_some() && base.is_file() {
            return Some(base);
        }
        pathext
            .split(';')
            .filter(|e| !e.is_empty())
            .map(|ext| {
                let mut p = base.clone().into_os_string();
                p.push(ext.to_ascii_lowercase());
                PathBuf::from(p)
            })
            .find(|p| p.is_file())
    };

    let as_path = Path::new(command);
    if as_path.components().count() > 1 || as_path.is_absolute() {
        return candidates(as_path.to_path_buf());
    }
    std::env::split_paths(path_var).find_map(|dir| candidates(dir.join(command)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exe_is_run_directly() {
        let line = command_line(Path::new(r"C:\Program Files\x\claude.exe"), &["--x".into()]);
        assert_eq!(line.program, r#""C:\Program Files\x\claude.exe""#);
        assert_eq!(line.args, ["--x"]);
        assert!(line.escape_args);
    }

    #[test]
    fn cmd_shim_goes_through_cmd() {
        let line = command_line(Path::new(r"C:\npm dir\claude.cmd"), &["a b".into()]);
        assert_eq!(line.args[..3], ["/d", "/s", "/c"]);
        assert_eq!(line.args[3], r#"""C:\npm dir\claude.cmd" "a b"""#);
        assert!(!line.escape_args);
    }

    #[test]
    fn finds_on_path_with_pathext() {
        let dir = std::env::temp_dir().join(format!("tethys-path-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.cmd"), "").unwrap();
        let path = std::env::join_paths([dir.clone()]).unwrap();
        assert_eq!(
            find_executable("agent", &path, ".EXE;.CMD"),
            Some(dir.join("agent.cmd"))
        );
        assert_eq!(find_executable("missing", &path, ".EXE;.CMD"), None);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
