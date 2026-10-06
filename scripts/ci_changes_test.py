"""Exercise CI selection against real Git history; no API or runner is mocked."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import textwrap
import unittest


SCRIPT = Path(__file__).with_name("ci_changes.py")
FULL = dict(agent_rust=True, rust_scope="workspace", frontend=True, bundle=True, helpers=True)
DOCS = dict(agent_rust=False, rust_scope="none", frontend=False, bundle=False, helpers=False)
AGENT = dict(DOCS, agent_rust=True, rust_scope="agent")
APP = dict(DOCS, rust_scope="workspace", frontend=True, bundle=True)


class SelectionTests(unittest.TestCase):
    def setUp(self):
        scratch = SCRIPT.resolve().parents[1] / "tmp"
        scratch.mkdir(exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(prefix="solador-ci-", dir=scratch)
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.repo = self.root / "repo"
        self.repo.mkdir()
        # Scratch repositories must not inherit workstation identity/hooks.
        self.env = dict(os.environ, GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1")
        self.git("init", "-q")
        self.git("config", "user.name", "CI tests")
        self.git("config", "user.email", "ci@example.invalid")
        self.git("config", "commit.gpgsign", "false")
        self.write("Cargo.toml", '[workspace.dependencies]\nshared = { path = "crates/shared" }\n')
        self.write("agent/Cargo.toml", '[dependencies]\nshared.workspace = true\n')
        self.write("crates/shared/Cargo.toml", '[build-dependencies]\nindirect = { path = "../indirect" }\n')
        self.write("crates/indirect/Cargo.toml", '[package]\nname = "indirect"\n')
        self.write("crates/app-only/Cargo.toml", '[package]\nname = "app-only"\n')
        self.write("agent/src/main.rs", "fn main() {}\n")
        self.write("app/ui/app.js", "// baseline\n")
        self.write("README.md", "baseline\n")
        self.base = self.commit()

    def git(self, *args):
        return subprocess.check_output(["git", "-C", str(self.repo), *args], text=True, env=self.env).strip()

    def write(self, path, value="changed\n"):
        dest = self.repo / path
        dest.parent.mkdir(parents=True, exist_ok=True)
        dest.write_text(value)

    def commit(self):
        self.git("add", "-A")
        self.git("commit", "-qm", "fixture", "--allow-empty")
        return self.git("rev-parse", "HEAD")

    def run_selection(self, event="push", payload=None):
        if payload is None:
            payload = {"before": self.base, "after": self.git("rev-parse", "HEAD")}
        event_file = self.root / "event.json"
        event_file.write_text(json.dumps(payload))
        output = self.root / "outputs"
        output.unlink(missing_ok=True)
        env = dict(self.env, GITHUB_OUTPUT=str(output), GITHUB_STEP_SUMMARY=str(self.root / "summary"))
        result = subprocess.run(
            [sys.executable, str(SCRIPT), "--repo", str(self.repo),
             "--event-name", event, "--event-path", str(event_file)],
            env=env, text=True, capture_output=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        selection = json.loads(result.stdout)["selection"]
        expected_outputs = {k: str(v).lower() for k, v in selection.items()}
        self.assertEqual(dict(line.split("=", 1) for line in output.read_text().splitlines()), expected_outputs)
        return selection

    def assert_change(self, paths, expected):
        for path in paths:
            self.write(path)
        self.commit()
        self.assertEqual(self.run_selection(), expected)

    def test_docs_only_skip_builds(self):
        self.assert_change(["README.md", "AGENTS.md", "CLAUDE.md", "agent/README.md", "docs/guide.md", "docs/image.png"], DOCS)

    def test_root_claude_import_is_docs_only(self):
        self.assert_change(["CLAUDE.md"], DOCS)

    def test_agent_code_excludes_app_work(self):
        self.assert_change(["agent/src/main.rs", "agent/tests/new_test.rs", "agent/README.md"], AGENT)

    def test_agent_build_script_rebuilds_agent(self):
        self.assert_change(["agent/build.rs"], AGENT)

    def test_agent_current_key_checks_both_consumers_and_release_tooling(self):
        self.assert_change(["agent/release-signing-key.pub"], FULL)

    def test_agent_standby_key_checks_both_consumers_and_release_tooling(self):
        self.assert_change(["agent/release-signing-key-next.pub"], FULL)

    def test_agent_deploy_runs_helpers_without_compilers(self):
        self.assert_change(["agent/deploy/install.sh", "agent/deploy/job.plist"], dict(DOCS, helpers=True))

    def test_frontend_only_does_not_build_native_bundles(self):
        self.assert_change(["app/ui/app.js", "app/ui/app.css"], dict(DOCS, frontend=True))

    def test_frontend_tests_select_browser_suite(self):
        self.assert_change(["tests/frontend/example.spec.js", "tests/frontend/package-lock.json"], dict(DOCS, frontend=True))

    def test_app_backend_selects_workspace_and_bundle(self):
        self.assert_change(["app/src-tauri/src/main.rs"], APP)

    def test_bundle_configuration_selects_workspace_and_bundle(self):
        self.assert_change(["app/src-tauri/tauri.conf.json", "app/src-tauri/icons/icon.png"], APP)

    def test_app_only_crate_does_not_build_linux_agent(self):
        self.assert_change(["crates/app-only/src/lib.rs"], APP)

    def test_transitive_agent_dependency_selects_both_products(self):
        self.assert_change(["crates/indirect/src/lib.rs"], dict(APP, agent_rust=True))

    def test_target_specific_path_dependency_is_followed(self):
        self.write("agent/Cargo.toml", '[target.\'cfg(target_os = "macos")\'.dependencies]\nnative = {path="../crates/native"}\n')
        self.write("crates/native/Cargo.toml", '[package]\nname="native"\n')
        self.base = self.commit()
        self.assert_change(["crates/native/src/lib.rs"], dict(APP, agent_rust=True))

    def test_mixed_changes_take_union(self):
        self.assert_change(["agent/src/main.rs", "app/ui/app.js"], dict(AGENT, frontend=True))

    def test_shared_build_inputs_force_full_suite(self):
        self.assert_change(["Cargo.lock", ".cargo/config.toml", "rust-toolchain.toml"], FULL)

    def test_workflow_or_selector_changes_force_full_suite(self):
        self.assert_change([".github/workflows/ci.yml", "scripts/ci_changes.py"], FULL)

    def test_unknown_paths_force_full_suite(self):
        self.assert_change(["new-component/input.dat"], FULL)

    def test_code_under_docs_is_not_ignored(self):
        self.assert_change(["docs/helper.py"], FULL)

    def test_agent_build_helper_checks_linking_and_scripts(self):
        self.assert_change(["scripts/build-agent.sh"], dict(AGENT, helpers=True))

    def test_app_build_helper_checks_bundle_and_scripts(self):
        self.assert_change(["scripts/build.sh"], dict(APP, helpers=True))

    def test_dmg_layout_test_runs_app_checks(self):
        self.assert_change(["scripts/dmg-layout-test.sh"], dict(APP, helpers=True))

    def test_shared_script_forces_full_suite(self):
        self.assert_change(["scripts/config.sh"], FULL)

    def test_deleted_code_is_selected(self):
        (self.repo / "agent/src/main.rs").unlink()
        self.commit()
        self.assertEqual(self.run_selection(), AGENT)

    def test_rename_out_of_code_keeps_source_coverage(self):
        self.write("docs/keep.md")
        self.git("mv", "agent/src/main.rs", "docs/renamed.md")
        self.commit()
        self.assertEqual(self.run_selection(), AGENT)

    def test_paths_are_not_truncated_at_api_file_limit(self):
        self.assert_change([f"docs/{n}.md" for n in range(310)] + ["agent/src/main.rs"], AGENT)

    def test_newline_in_filename_does_not_hide_code(self):
        self.assert_change(["agent/src/odd\nname.rs"], AGENT)

    def test_pr_uses_merge_base_not_unrelated_base_changes(self):
        self.write("docs/pr.md")
        head = self.commit()
        self.git("checkout", "-q", "--detach", self.base)
        self.write("Cargo.lock")
        new_base = self.commit()
        payload = {"pull_request": {"base": {"sha": new_base}, "head": {"sha": head}}}
        self.assertEqual(self.run_selection("pull_request", payload), DOCS)

    def test_merge_group_includes_all_group_changes(self):
        self.write("agent/src/main.rs")
        self.commit()
        self.write("app/ui/app.js")
        head = self.commit()
        payload = {"merge_group": {"base_sha": self.base, "head_sha": head}}
        self.assertEqual(self.run_selection("merge_group", payload), dict(AGENT, frontend=True))

    def test_missing_or_zero_base_falls_back_to_full(self):
        for payload in ({}, {"before": "0" * 40, "after": self.base}, {"before": "a" * 40, "after": self.base}):
            with self.subTest(payload=payload):
                self.assertEqual(self.run_selection(payload=payload), FULL)

    def test_untrusted_revision_is_not_a_git_option(self):
        self.assertEqual(self.run_selection(payload={"before": "--help", "after": self.base}), FULL)

    def test_manual_or_unknown_event_runs_full_suite(self):
        self.assertEqual(self.run_selection("workflow_dispatch", {}), FULL)
        self.assertEqual(self.run_selection("unrecognized", {}), FULL)

    def test_broken_dependency_graph_falls_back_to_full(self):
        self.write("crates/shared/Cargo.toml", "not valid TOML [")
        self.base = self.commit()
        self.assert_change(["crates/indirect/src/lib.rs"], FULL)

    def test_empty_diff_needs_only_lightweight_checks(self):
        self.commit()
        self.assertEqual(self.run_selection(), DOCS)


class RequiredGuardTests(unittest.TestCase):
    """Execute the actual workflow guard, including failure/cancellation cases."""

    def run_guard(self, result="success", outputs=None):
        workflow = (SCRIPT.resolve().parents[1] / ".github/workflows/ci.yml").read_text()
        code = workflow.split("python3 -B - <<'PYTHON'\n", 1)[1].split("          PYTHON", 1)[0]
        outputs = {key: str(value).lower() for key, value in DOCS.items()} if outputs is None else outputs
        return subprocess.run(
            [sys.executable, "-c", textwrap.dedent(code)], capture_output=True,
            env=dict(os.environ, SELECT_RESULT=result, SELECT_OUTPUTS=json.dumps(outputs)),
        ).returncode

    def test_valid_docs_selection_passes(self):
        self.assertEqual(self.run_guard(), 0)

    def test_valid_full_selection_passes(self):
        self.assertEqual(self.run_guard(outputs={k: str(v).lower() for k, v in FULL.items()}), 0)

    def test_failed_cancelled_and_skipped_selection_blocks_merge(self):
        for result in ("failure", "cancelled", "skipped", ""):
            with self.subTest(result=result):
                self.assertNotEqual(self.run_guard(result=result), 0)

    def test_missing_or_malformed_outputs_block_merge(self):
        valid = {k: str(v).lower() for k, v in DOCS.items()}
        for outputs in ({}, dict(valid, frontend="maybe"), dict(valid, rust_scope="unknown"),
                        dict(valid, unexpected="true"), {k: v for k, v in valid.items() if k != "bundle"}):
            with self.subTest(outputs=outputs):
                self.assertNotEqual(self.run_guard(outputs=outputs), 0)


if __name__ == "__main__":
    unittest.main()
