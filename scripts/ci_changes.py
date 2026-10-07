#!/usr/bin/env python3
"""Select CI work from a complete Git diff. Requires Python 3.11+, no packages.

Unknown paths, missing history and unreadable dependency graphs select everything.
The workflow never uses trigger-level path filters: required jobs still report,
and Secrets guard rejects a failed selector or invalid/missing outputs.
"""

import argparse
import json
import os
from pathlib import Path, PurePosixPath
import re
import subprocess
import tomllib


FULL = dict(agent_rust=True, rust_scope="workspace", frontend=True, bundle=True, helpers=True)
EMPTY = dict(agent_rust=False, rust_scope="none", frontend=False, bundle=False, helpers=False)
AGENT_HELPERS = {
    "scripts/agent-signing.sh", "scripts/agent-standby-key.sh",
    "scripts/agent-feed-guard.sh", "scripts/agent-feed-guard-test.sh",
    "scripts/agent-deps-guard.sh",
}
APP_HELPERS = {
    "scripts/build.sh", "scripts/run.sh", "scripts/run-test.sh",
    "scripts/signing-identity-test.sh", "scripts/dmg-layout-test.sh",
    "scripts/notarize-outcome-test.sh",
    "scripts/generate-icons.sh", "scripts/render-icons.mjs",
}


def git(repo, *args):
    return subprocess.check_output(["git", "-C", str(repo), *args], stderr=subprocess.PIPE)


def changed_paths(repo, event_name, event):
    if event_name == "pull_request":
        base, head = event["pull_request"]["base"]["sha"], event["pull_request"]["head"]["sha"]
    elif event_name == "merge_group":
        base, head = event["merge_group"]["base_sha"], event["merge_group"]["head_sha"]
    elif event_name == "push":
        base, head = event["before"], event["after"]
    else:
        raise ValueError("manual or unknown event")
    for sha in (base, head):
        if not isinstance(sha, str) or not re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", sha) or set(sha) == {"0"}:
            raise ValueError("missing or invalid commit")
        git(repo, "cat-file", "-e", sha + "^{commit}")
    if event_name == "pull_request":
        base = git(repo, "merge-base", base, head).decode().strip()
    # No API file cap, no rename elision, and no line-based filename splitting.
    return [os.fsdecode(p) for p in git(repo, "diff", "--name-only", "--no-renames", "-z", base, head, "--").split(b"\0") if p]


def agent_dependency_roots(repo):
    """Follow all local edges, including build/dev and platform-specific ones."""
    with (repo / "Cargo.toml").open("rb") as stream:
        workspace_deps = tomllib.load(stream).get("workspace", {}).get("dependencies", {})
    pending = [repo / "agent" / "Cargo.toml"]
    seen = set()
    while pending:
        manifest = pending.pop().resolve()
        manifest.relative_to(repo)  # An external path cannot be classified safely.
        if manifest in seen:
            continue
        seen.add(manifest)
        with manifest.open("rb") as stream:
            package = tomllib.load(stream)
        tables = [package, *package.get("target", {}).values()]
        for table in tables:
            for kind in ("dependencies", "build-dependencies", "dev-dependencies"):
                for name, dependency in table.get(kind, {}).items():
                    origin = manifest.parent
                    if isinstance(dependency, dict) and dependency.get("workspace"):
                        dependency = workspace_deps[name]
                        origin = repo
                    if isinstance(dependency, dict) and "path" in dependency:
                        pending.append(origin / dependency["path"] / "Cargo.toml")
    return {p.parent.relative_to(repo).as_posix() + "/" for p in seen}


def is_documentation(path):
    p = PurePosixPath(path)
    if path in {"README.md", "AGENTS.md", "CLAUDE.md", "CHANGELOG.md", "CONTRIBUTING.md", "SECURITY.md", "LICENSE", "LICENSE.md"}:
        return True
    if p.name in {"README.md", "AGENTS.md"} and (
        p.parent.as_posix() in {"agent", "app"}
        or (len(p.parts) == 3 and p.parts[0] == "crates")
    ):
        return True
    return path.startswith("docs/") and p.suffix.lower() in {".md", ".png", ".jpg", ".jpeg", ".svg", ".webp", ".pdf"}


def select_paths(repo, paths):
    selected = dict(EMPTY)
    dependency_roots = None
    for path in paths:
        # Both products and release tooling consume the agent trust set.
        if path in {"agent/release-signing-key.pub", "agent/release-signing-key-next.pub"}:
            return dict(FULL)
        if is_documentation(path):
            continue
        if path.startswith("agent/deploy/") or path in AGENT_HELPERS:
            selected["helpers"] = True
        elif path.startswith("agent/") or path == "scripts/build-agent.sh":
            selected["agent_rust"] = True
            if selected["rust_scope"] == "none":
                selected["rust_scope"] = "agent"
            if path.startswith("scripts/"):
                selected["helpers"] = True
        elif path.startswith(("app/ui/", "tests/frontend/")):
            selected["frontend"] = True
        elif path.startswith("app/src-tauri/") or path in APP_HELPERS:
            selected.update(rust_scope="workspace", frontend=True, bundle=True)
            if path.startswith("scripts/"):
                selected["helpers"] = True
        elif path.startswith("crates/"):
            selected.update(rust_scope="workspace", frontend=True, bundle=True)
            if dependency_roots is None:
                dependency_roots = agent_dependency_roots(repo)
            if any(path.startswith(root) for root in dependency_roots):
                selected["agent_rust"] = True
        else:
            return dict(FULL)
    return selected


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=Path.cwd())
    parser.add_argument("--event-name", default=os.environ.get("GITHUB_EVENT_NAME", ""))
    parser.add_argument("--event-path", type=Path, default=os.environ.get("GITHUB_EVENT_PATH"))
    args = parser.parse_args()
    try:
        event = json.loads(args.event_path.read_text()) if args.event_path else {}
        paths = changed_paths(args.repo, args.event_name, event)
        selection = select_paths(args.repo.resolve(), paths)
        reason = f"Classified all {len(paths)} changed paths. Unrecognized paths select the full suite."
    except (OSError, ValueError, KeyError, TypeError, subprocess.CalledProcessError):
        selection = dict(FULL)
        reason = "Could not establish a complete diff and dependency graph, or this is a manual run: full suite selected."

    # Only fixed keys and validated constant values reach Actions outputs.
    if output := os.environ.get("GITHUB_OUTPUT"):
        with open(output, "a") as stream:
            for key, value in selection.items():
                stream.write(f"{key}={str(value).lower()}\n")
    if summary := os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(summary, "a") as stream:
            stream.write(f"### CI selection\n\n{reason}\n\n| Work | Selection |\n| --- | --- |\n")
            for key, value in selection.items():
                stream.write(f"| {key} | {str(value).lower()} |\n")
    print(json.dumps({"selection": selection, "reason": reason}))


if __name__ == "__main__":
    main()
