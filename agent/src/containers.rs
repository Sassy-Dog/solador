//! Container/VM enumeration by shelling out to `podman`, `docker`, and `tart`.
//!
//! Each runtime is optional: if its binary is not on `PATH`, it is silently
//! skipped. All pure parsing is factored into free functions so it can be unit
//! tested against representative sample output.

use std::process::Command;

use serde::Serialize;

/// One container or VM, in the exact wire shape the original decoder expects.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Container {
    pub name: String,
    #[serde(rename = "statusText")]
    pub status_text: String,
    #[serde(rename = "isRunning")]
    pub is_running: bool,
    /// One of `"docker"`, `"podman"`, `"tart"`.
    pub runtime: String,
    /// Image reference; `null` for tart VMs.
    pub image: Option<String>,
}

/// Collect containers from every available runtime and return the merged list.
pub fn list_containers() -> Vec<Container> {
    let mut out = Vec::new();

    if let Some(stdout) = run_format("podman") {
        out.extend(parse_ps_output(&stdout, "podman"));
    }
    if let Some(stdout) = run_format("docker") {
        out.extend(parse_ps_output(&stdout, "docker"));
    }
    if let Some(stdout) = run_tart() {
        out.extend(parse_tart_output(&stdout));
    }

    out
}

/// Run `<runtime> ps -a --format '{{.Names}}|{{.Status}}|{{.Image}}'`.
/// Returns `None` if the binary is missing or the command fails.
fn run_format(runtime: &str) -> Option<String> {
    run_command(
        runtime,
        &["ps", "-a", "--format", "{{.Names}}|{{.Status}}|{{.Image}}"],
    )
}

/// Run `tart list`. Returns `None` if missing or failed.
fn run_tart() -> Option<String> {
    run_command("tart", &["list"])
}

/// Run a runtime command, returning its stdout on success.
///
/// A *missing* binary (the runtime simply isn't installed) stays silent — that's
/// the expected case for an optional runtime. Any other failure — a spawn error
/// that isn't `NotFound`, or a non-zero exit — is logged via `tracing::warn!`
/// with the runtime name, exit status, and trimmed stderr, so an operator can
/// diagnose e.g. a stopped rootless-podman socket from `journalctl`.
fn run_command(runtime: &str, args: &[&str]) -> Option<String> {
    let output = match Command::new(runtime).args(args).output() {
        Ok(o) => o,
        Err(e) => {
            // Binary not on PATH is the normal "runtime not installed" case.
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(runtime, error = %e, "failed to run container runtime");
            }
            return None;
        }
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!(
            runtime,
            status = %output.status,
            stderr = stderr.trim(),
            "container runtime command exited non-zero"
        );
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Parse pipe-delimited `name|status|image` lines from podman/docker.
///
/// `isRunning` is true when the status begins with `Up`. Blank lines are skipped.
fn parse_ps_output(stdout: &str, runtime: &str) -> Vec<Container> {
    stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| {
            let mut parts = line.splitn(3, '|');
            let name = parts.next()?.trim();
            if name.is_empty() {
                return None;
            }
            let status = parts.next().unwrap_or("").trim();
            let image_raw = parts.next().unwrap_or("").trim();
            let image = if image_raw.is_empty() {
                None
            } else {
                Some(image_raw.to_string())
            };
            Some(Container {
                name: name.to_string(),
                status_text: status.to_string(),
                is_running: status.starts_with("Up"),
                runtime: runtime.to_string(),
                image,
            })
        })
        .collect()
}

/// Parse the tabular output of `tart list`.
///
/// Format (whitespace-separated columns):
/// ```text
/// Source Name        Disk Size State
/// local  my-vm       25   ...   running
/// ```
/// We take the VM name from the 2nd column and state from the LAST column; this
/// is lenient about the number of intermediate columns. The header row (whose
/// 2nd column is literally `Name`) and blank lines are skipped.
fn parse_tart_output(stdout: &str) -> Vec<Container> {
    stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| {
            let cols: Vec<&str> = line.split_whitespace().collect();
            // Need at least: <source> <name> ... <state>
            if cols.len() < 3 {
                return None;
            }
            let name = cols[1];
            let state = *cols.last().unwrap();
            // Skip header row.
            if name.eq_ignore_ascii_case("Name") || state.eq_ignore_ascii_case("State") {
                return None;
            }
            Some(Container {
                name: name.to_string(),
                status_text: state.to_string(),
                is_running: state.eq_ignore_ascii_case("running"),
                runtime: "tart".to_string(),
                image: None,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_podman_pipe_output() {
        let sample = "\
llm|Up 2 days|llama-swap:latest
db|Exited (0) 3 hours ago|postgres:16
cache|Up 5 minutes (healthy)|redis:7
";
        let got = parse_ps_output(sample, "podman");
        assert_eq!(got.len(), 3);

        assert_eq!(got[0].name, "llm");
        assert_eq!(got[0].status_text, "Up 2 days");
        assert!(got[0].is_running);
        assert_eq!(got[0].runtime, "podman");
        assert_eq!(got[0].image.as_deref(), Some("llama-swap:latest"));

        assert_eq!(got[1].name, "db");
        assert!(!got[1].is_running);
        assert_eq!(got[1].image.as_deref(), Some("postgres:16"));

        assert!(got[2].is_running); // "Up 5 minutes (healthy)"
    }

    #[test]
    fn parses_docker_pipe_output_with_runtime_label() {
        let sample = "web|Up About an hour|nginx:alpine\n";
        let got = parse_ps_output(sample, "docker");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].runtime, "docker");
        assert!(got[0].is_running);
    }

    #[test]
    fn ps_output_skips_blank_lines_and_handles_missing_image() {
        let sample = "\n\nlonely|Up 1 second|\n\n";
        let got = parse_ps_output(sample, "podman");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "lonely");
        assert_eq!(got[0].image, None);
        assert!(got[0].is_running);
    }

    #[test]
    fn parses_tart_tabular_output() {
        // Representative `tart list` output: header + two VMs.
        let sample = "\
Source Name        Disk Size State
local  build-runner 50   12   running
local  macos-ci     50   8    stopped
";
        let got = parse_tart_output(sample);
        assert_eq!(got.len(), 2);

        assert_eq!(got[0].name, "build-runner");
        assert_eq!(got[0].status_text, "running");
        assert!(got[0].is_running);
        assert_eq!(got[0].runtime, "tart");
        assert_eq!(got[0].image, None);

        assert_eq!(got[1].name, "macos-ci");
        assert!(!got[1].is_running);
    }

    #[test]
    fn tart_skips_header_and_blank_lines() {
        let sample = "Source Name State\n\n";
        let got = parse_tart_output(sample);
        assert!(got.is_empty(), "header-only output should yield nothing");
    }

    #[test]
    fn container_serializes_to_exact_wire_keys() {
        let c = Container {
            name: "llm".to_string(),
            status_text: "Up 2 days".to_string(),
            is_running: true,
            runtime: "podman".to_string(),
            image: Some("llama-swap:latest".to_string()),
        };
        let v = serde_json::to_value(&c).unwrap();
        let obj = v.as_object().unwrap();
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec!["image", "isRunning", "name", "runtime", "statusText"]
        );
        assert!(v["isRunning"].is_boolean());
        assert!(v["image"].is_string());

        // Null image must serialize to JSON null, not omit the key.
        let mut c2 = c.clone();
        c2.image = None;
        let v2 = serde_json::to_value(&c2).unwrap();
        assert!(v2["image"].is_null());
        assert!(v2.as_object().unwrap().contains_key("image"));
    }
}
