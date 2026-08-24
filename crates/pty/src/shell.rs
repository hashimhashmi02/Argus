//! Finding a shell to run.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// A resolved shell: the executable plus the arguments Argus wants it started
/// with.
#[derive(Debug, Clone)]
pub struct Shell {
    pub program: PathBuf,
    pub args: Vec<OsString>,
}

/// Candidates in descending order of preference.
///
/// PowerShell 7 (`pwsh.exe`) first because it is the modern shell and is what a
/// developer who has installed it expects to get. Windows PowerShell 5.1
/// (`powershell.exe`) second because it ships with every Windows install, so it
/// is the reliable floor. `cmd.exe` last: it is a poor interactive shell, but it
/// is the one binary that cannot be missing, so it is the backstop that keeps
/// Argus from having no shell at all.
const CANDIDATES: &[(&str, &[&str])] = &[
    ("pwsh.exe", &["-NoLogo"]),
    ("powershell.exe", &["-NoLogo"]),
    ("cmd.exe", &[]),
];

/// Find the best available shell.
///
/// Resolution walks `PATH` explicitly rather than handing a bare name to the
/// spawner and hoping. Two reasons: we want to *know* which shell we picked so
/// it can be reported and logged, and resolving up front turns "this shell is
/// not installed" into an ordinary `None` at a point where we can still fall
/// back, instead of an opaque spawn failure later.
pub fn resolve_shell() -> Option<Shell> {
    for (exe, args) in CANDIDATES {
        if let Some(program) = which(exe) {
            return Some(Shell {
                program,
                args: args.iter().map(OsString::from).collect(),
            });
        }
    }
    None
}

/// Minimal `which`: the first directory containing `exe` as a regular file.
///
/// Deliberately not a dependency. Windows `PATHEXT` resolution is a real rabbit
/// hole, but every candidate above is named with its `.exe` suffix already, so
/// a plain existence check is exactly correct here and nothing more is needed.
///
/// `PATH` is searched first, then the fixed system directories. The fallback is
/// not belt-and-braces: under an emulation layer such as MSYS2 or Git Bash,
/// `PATH` is handed to us in POSIX form (`/c/Windows/System32`, colon-separated)
/// which `split_paths` correctly refuses to parse as Windows paths. Without the
/// fallback, Argus would report "no shell installed" on a machine that plainly
/// has one, purely because of the shell it happened to be launched from.
fn which(exe: &str) -> Option<PathBuf> {
    let from_path = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>());

    from_path
        .chain(system_dirs())
        .map(|dir| dir.join(exe))
        .find(|candidate| is_file(candidate))
}

/// Directories that always exist on Windows and always hold a shell.
///
/// Derived from `SystemRoot` rather than hardcoded to `C:\Windows`, because
/// Windows genuinely can be installed on another volume and the environment
/// variable is the supported way to ask.
fn system_dirs() -> Vec<PathBuf> {
    let Some(root) = std::env::var_os("SystemRoot") else {
        return Vec::new();
    };
    let system32 = PathBuf::from(root).join("System32");
    vec![system32.join("WindowsPowerShell").join("v1.0"), system32]
}

fn is_file(p: &Path) -> bool {
    p.metadata().map(|m| m.is_file()).unwrap_or(false)
}
