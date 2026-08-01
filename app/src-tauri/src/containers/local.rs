//! Finding and running the local container tools — the only part of container
//! discovery that touches the machine.
//!
//! Port of the I/O half of `LocalContainerService`
//! (`DevCanopy/Services/Containers/LocalContainerService.swift`). Everything it
//! produces is parsed by [`super::parse`], which is where the tested logic
//! lives.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::parse::{merge, parse_ps_output, parse_tart_list, LocalRuntime, MergeOutcome};

/// `docker ps` / `podman ps`, formatted into the pipe-delimited shape
/// [`parse_ps_output`] reads. `-a` so stopped containers are listed too: an
/// exited container that should be running is exactly what the panel exists to
/// show.
const PS_ARGS: [&str; 4] = ["ps", "-a", "--format", "{{.Names}}|{{.Status}}|{{.Image}}"];

/// Directories probed for tool executables, in order.
///
/// **Absolute paths, not `PATH`**, and that is deliberate on macOS: a GUI app
/// inherits a minimal environment from `launchd`, so `PATH` there routinely
/// lacks `/opt/homebrew/bin` and every Homebrew-installed tool would read as
/// "not installed". Same list as the Swift service's.
#[cfg(not(windows))]
fn search_dirs() -> Vec<PathBuf> {
    ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"]
        .iter()
        .map(PathBuf::from)
        .collect()
}

/// On Windows there is no equivalent well-known install prefix — Docker
/// Desktop, Podman Desktop and the package managers all land elsewhere — and a
/// GUI process inherits the user's real `PATH`, so `PATH` *is* the answer here.
#[cfg(windows)]
fn search_dirs() -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default()
}

/// The file names a tool could have on this platform.
#[cfg(windows)]
fn candidate_names(tool: &str) -> Vec<String> {
    vec![format!("{tool}.exe")]
}

#[cfg(not(windows))]
fn candidate_names(tool: &str) -> Vec<String> {
    vec![tool.to_owned()]
}

/// Whether a path is something we could actually run.
#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// First match for `tool` across `dirs`, in order.
fn locate(tool: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    for dir in dirs {
        for name in candidate_names(tool) {
            let candidate = dir.join(&name);
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

/// Absolute path of a container tool on this machine, or `None` when it is not
/// installed.
pub fn tool_path(tool: &str) -> Option<PathBuf> {
    locate(tool, &search_dirs())
}

/// Whether this runtime is worth probing for here.
///
/// Tart is a macOS-only VM manager (it runs on Apple's Virtualization
/// framework), so probing for it on Windows is guaranteed to fail and would
/// only add a "not installed" runtime to reason about.
fn probed_here(runtime: LocalRuntime) -> bool {
    match runtime {
        LocalRuntime::Tart => cfg!(target_os = "macos"),
        LocalRuntime::Docker | LocalRuntime::Podman => true,
    }
}

/// One local discovery pass: which runtimes exist, and what each one said.
pub struct LocalPoll {
    /// Runtimes actually installed here, in probe order. An empty list is what
    /// makes the panel say "no container runtimes" rather than "no containers"
    /// — a machine with no tooling is not a machine with nothing running.
    pub detected: Vec<LocalRuntime>,
    /// Per-runtime result; `None` is a failed invocation, which
    /// [`merge`] answers with that runtime's last-known list.
    pub results: Vec<(LocalRuntime, Option<Vec<wire::Container>>)>,
}

impl LocalPoll {
    /// Folds this pass into the previous one's last-known lists.
    pub fn merge_with(
        self,
        last_known: std::collections::BTreeMap<LocalRuntime, Vec<wire::Container>>,
    ) -> (Vec<LocalRuntime>, MergeOutcome) {
        (self.detected, merge(self.results, last_known))
    }
}

/// Runs whichever tools are installed and parses their output.
///
/// Blocking (it spawns processes and waits), so callers run it off the async
/// executor. Never fails as a whole: a missing tool is skipped and an erroring
/// one contributes `None`, which the merge turns into last-known rows rather
/// than a blank panel.
pub fn poll() -> LocalPoll {
    let mut detected = Vec::new();
    let mut results = Vec::new();

    for runtime in LocalRuntime::ALL {
        if !probed_here(runtime) {
            continue;
        }
        let Some(path) = tool_path(runtime.id()) else {
            continue;
        };
        detected.push(runtime);
        let parsed = match runtime {
            LocalRuntime::Tart => run(&path, &["list"]).map(|out| parse_tart_list(&out)),
            docker_or_podman => {
                run(&path, &PS_ARGS).map(|out| parse_ps_output(&out, docker_or_podman))
            }
        };
        results.push((runtime, parsed));
    }

    LocalPoll { detected, results }
}

/// Runs a tool and captures stdout. `None` on any failure — a spawn error, a
/// non-zero exit, or output that is not UTF-8.
///
/// stderr is discarded rather than surfaced: `docker ps` against a stopped
/// daemon writes a multi-line diagnostic that would swamp a one-line panel
/// footer, and the footer already says which tool could not be read.
fn run(executable: &Path, arguments: &[&str]) -> Option<String> {
    let mut command = Command::new(executable);
    command
        .args(arguments)
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    // Without this a console window flashes on screen for every poll — every
    // 10 seconds, forever, in an app meant to sit on a second monitor.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tart_is_probed_on_macos_only() {
        assert_eq!(probed_here(LocalRuntime::Tart), cfg!(target_os = "macos"));
        assert!(probed_here(LocalRuntime::Docker));
        assert!(probed_here(LocalRuntime::Podman));
    }

    #[test]
    fn a_tool_that_is_not_installed_locates_as_none() {
        let dir = tempfile::tempdir().expect("temp dir");
        assert_eq!(locate("docker", &[dir.path().to_path_buf()]), None);
    }

    #[cfg(unix)]
    #[test]
    fn locate_prefers_the_first_directory_that_has_the_tool() {
        use std::os::unix::fs::PermissionsExt;

        let first = tempfile::tempdir().expect("temp dir");
        let second = tempfile::tempdir().expect("temp dir");
        let write_tool = |dir: &std::path::Path| {
            let path = dir.join("docker");
            std::fs::write(&path, "#!/bin/sh\n").expect("write");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
            path
        };
        let earlier = write_tool(first.path());
        write_tool(second.path());

        let dirs = vec![first.path().to_path_buf(), second.path().to_path_buf()];
        assert_eq!(locate("docker", &dirs), Some(earlier));
    }

    #[cfg(unix)]
    #[test]
    fn a_non_executable_file_is_not_a_tool() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("docker"), "not a program").expect("write");
        assert_eq!(
            locate("docker", &[dir.path().to_path_buf()]),
            None,
            "a readable file that cannot be run is not an installed tool"
        );
    }

    #[test]
    fn a_failed_invocation_reads_as_none_rather_than_empty_output() {
        // `false`-style exit codes must not read as "this runtime has no
        // containers" — that is the difference between retaining rows and
        // blanking them.
        let shell = PathBuf::from(if cfg!(windows) {
            "C:\\Windows\\System32\\cmd.exe"
        } else {
            "/bin/sh"
        });
        if !shell.exists() {
            return;
        }
        let args: Vec<&str> = if cfg!(windows) {
            vec!["/C", "exit 1"]
        } else {
            vec!["-c", "exit 1"]
        };
        assert_eq!(run(&shell, &args), None);
    }

    #[test]
    fn polling_a_machine_never_panics_whatever_is_installed() {
        // The real probe, on whatever this machine has: the contract is that
        // it always answers, and that every detected runtime produced a result.
        let poll = poll();
        assert_eq!(poll.detected.len(), poll.results.len());
        for (runtime, _) in &poll.results {
            assert!(poll.detected.contains(runtime));
        }
    }
}
