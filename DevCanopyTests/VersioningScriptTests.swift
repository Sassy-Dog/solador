import Foundation
import XCTest

/// Exercises the versioning capability scripts (`Scripts/get-version-info.sh`,
/// `Scripts/get-build-number.sh`) against throwaway git fixtures — the org
/// Versioning spec's (v1.0, §3) mandatory list: the patch floor, the
/// month-roll reset, the §4 mint collision replay, idempotent re-runs, and
/// the §6 semver→CalVer monotonicity vector. Each test builds a hermetic
/// bare-origin + working-clone pair so the remote-visible `ls-remote` probe
/// is exercised for real.
final class VersioningScriptTests: XCTestCase {
    // MARK: - Fixture plumbing

    /// Repo root derived from this source file's compile-time path
    /// (`<root>/DevCanopyTests/VersioningScriptTests.swift`).
    private static let repoRoot = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent() // DevCanopyTests/
        .deletingLastPathComponent() // repo root

    private static let versionScript = repoRoot.appendingPathComponent("Scripts/get-version-info.sh").path
    private static let buildNumberScript = repoRoot.appendingPathComponent("Scripts/get-build-number.sh").path

    private var fixtureDir: URL!
    private var workDir: URL!

    override func setUpWithError() throws {
        fixtureDir = FileManager.default.temporaryDirectory
            .appendingPathComponent("VersioningScriptTests-\(UUID().uuidString)")
        let originDir = fixtureDir.appendingPathComponent("origin.git")
        workDir = fixtureDir.appendingPathComponent("work")
        try FileManager.default.createDirectory(at: originDir, withIntermediateDirectories: true)
        try FileManager.default.createDirectory(at: workDir, withIntermediateDirectories: true)
        try git(["init", "--bare", "--quiet"], cwd: originDir)
        try git(["init", "--quiet"], cwd: workDir)
        try git(["remote", "add", "origin", originDir.path], cwd: workDir)
    }

    override func tearDownWithError() throws {
        if let fixtureDir {
            try? FileManager.default.removeItem(at: fixtureDir)
        }
    }

    private struct ProcessResult {
        let status: Int32
        let stdout: String
        let stderr: String
    }

    /// Environment for every spawned process: the host env minus the org
    /// version seams (so a developer's shell can never leak a pin into a
    /// test), plus a hermetic git identity/config (no host tag-signing bleed).
    private func baseEnvironment() -> [String: String] {
        var env = ProcessInfo.processInfo.environment
        for seam in ["MARKETING_VERSION", "BUILD_NUMBER", "VERSION_DATE_OVERRIDE", "VERSION_PATCH_OVERRIDE"] {
            env.removeValue(forKey: seam)
        }
        env["GIT_CONFIG_GLOBAL"] = "/dev/null"
        env["GIT_CONFIG_SYSTEM"] = "/dev/null"
        env["GIT_AUTHOR_NAME"] = "Versioning Tests"
        env["GIT_AUTHOR_EMAIL"] = "tests@example.invalid"
        env["GIT_COMMITTER_NAME"] = "Versioning Tests"
        env["GIT_COMMITTER_EMAIL"] = "tests@example.invalid"
        return env
    }

    @discardableResult
    private func runProcess(
        _ executable: String,
        _ arguments: [String],
        cwd: URL,
        env extraEnv: [String: String] = [:]
    ) throws -> ProcessResult {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: executable)
        process.arguments = arguments
        process.currentDirectoryURL = cwd
        process.environment = baseEnvironment().merging(extraEnv) { _, new in new }
        let outPipe = Pipe()
        let errPipe = Pipe()
        process.standardOutput = outPipe
        process.standardError = errPipe
        try process.run()
        // Drain both pipes to EOF before waiting, to avoid pipe-buffer deadlock.
        let outData = outPipe.fileHandleForReading.readDataToEndOfFile()
        let errData = errPipe.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        return ProcessResult(
            status: process.terminationStatus,
            stdout: String(data: outData, encoding: .utf8) ?? "",
            stderr: String(data: errData, encoding: .utf8) ?? ""
        )
    }

    @discardableResult
    private func git(_ arguments: [String], cwd: URL, env: [String: String] = [:]) throws -> ProcessResult {
        let result = try runProcess("/usr/bin/git", arguments, cwd: cwd, env: env)
        XCTAssertEqual(result.status, 0, "git \(arguments.joined(separator: " ")) failed: \(result.stderr)")
        return result
    }

    /// Runs one of the versioning scripts inside the fixture work repo.
    @discardableResult
    private func script(
        _ path: String,
        _ arguments: [String] = [],
        env: [String: String] = [:]
    ) throws -> ProcessResult {
        try runProcess("/bin/bash", [path] + arguments, cwd: workDir, env: env)
    }

    /// Creates an empty commit with a fixed author + committer date (UTC).
    /// `git rev-list --since` filters on the committer date — exactly what
    /// the marketing patch counts.
    private func commit(_ message: String, date: String) throws {
        try git(
            ["commit", "--allow-empty", "--quiet", "-m", message],
            cwd: workDir,
            env: ["GIT_AUTHOR_DATE": date, "GIT_COMMITTER_DATE": date]
        )
    }

    // MARK: - Marketing version (§2): floor, month-roll reset, idempotency

    func testPatchFloorsAtOneWhenMonthHasNoCommits() throws {
        try commit("june work", date: "2026-06-15T12:00:00Z")
        let result = try script(Self.versionScript, ["--version"], env: ["VERSION_DATE_OVERRIDE": "2026-07-10"])
        XCTAssertEqual(result.status, 0, result.stderr)
        XCTAssertEqual(result.stdout.trimmed, "2026.7.1")
    }

    func testPatchCountsCommitsThisMonthAndResetsOnTheFirst() throws {
        try commit("one", date: "2026-06-05T09:00:00Z")
        try commit("two", date: "2026-06-06T09:00:00Z")
        try commit("three", date: "2026-06-07T09:00:00Z")

        let june = try script(Self.versionScript, ["--version"], env: ["VERSION_DATE_OVERRIDE": "2026-06-20"])
        XCTAssertEqual(june.stdout.trimmed, "2026.6.3")

        // Month roll: identical history viewed on July 1 — the patch resets
        // (floored at 1) and the month is non-padded (7, never 07).
        let july = try script(Self.versionScript, ["--version"], env: ["VERSION_DATE_OVERRIDE": "2026-07-01"])
        XCTAssertEqual(july.stdout.trimmed, "2026.7.1")
    }

    func testVersionIsIdempotentForSameCommitAndDate() throws {
        try commit("work", date: "2026-06-05T09:00:00Z")
        let env = ["VERSION_DATE_OVERRIDE": "2026-06-20"]
        let first = try script(Self.versionScript, ["--version"], env: env)
        let second = try script(Self.versionScript, ["--version"], env: env)
        XCTAssertEqual(first.stdout, second.stdout, "same commit + same UTC day must yield the same version")
    }

    func testMarketingVersionPinIsEmittedVerbatim() throws {
        try commit("work", date: "2026-06-05T09:00:00Z")
        let result = try script(Self.versionScript, ["--version"], env: ["MARKETING_VERSION": "2026.3.9"])
        XCTAssertEqual(result.stdout.trimmed, "2026.3.9")
    }

    // MARK: - §4 mint: collision replay, idempotent reuse, fail-closed

    /// The spec's mandatory collision replay: a post-month-roll release of a
    /// prior-month commit mints vYYYY.M.1 (count 0 → floored to 1); the
    /// month's first real commit also resolves .1 and must BUMP to .2 — two
    /// releases, two distinct versions, no bare-skip, no bare-fail.
    func testMintCollisionReplayBumpsPastMonthRollFloor() throws {
        let dateEnv = ["VERSION_DATE_OVERRIDE": "2026-07-02"]

        try commit("june work", date: "2026-06-15T12:00:00Z")
        let first = try script(Self.versionScript, ["--tag", "--push"], env: dateEnv)
        XCTAssertEqual(first.status, 0, first.stderr)
        XCTAssertTrue(first.stdout.contains("version=2026.7.1"), first.stdout)
        XCTAssertTrue(first.stdout.contains("action=create"), first.stdout)

        try commit("first july commit", date: "2026-07-02T08:00:00Z")
        let second = try script(Self.versionScript, ["--tag", "--push"], env: dateEnv)
        XCTAssertEqual(second.status, 0, second.stderr)
        XCTAssertTrue(second.stdout.contains("version=2026.7.2"), second.stdout)
        XCTAssertTrue(second.stdout.contains("tag=v2026.7.2"), second.stdout)
        XCTAssertTrue(second.stdout.contains("action=create"), second.stdout)
    }

    func testMintReusesTagOnIdempotentRerun() throws {
        let dateEnv = ["VERSION_DATE_OVERRIDE": "2026-07-02"]
        try commit("july commit", date: "2026-07-02T08:00:00Z")

        let first = try script(Self.versionScript, ["--tag", "--push"], env: dateEnv)
        XCTAssertEqual(first.status, 0, first.stderr)
        XCTAssertTrue(first.stdout.contains("version=2026.7.1"), first.stdout)
        XCTAssertTrue(first.stdout.contains("action=create"), first.stdout)

        // Re-running the mint for the same commit must not fail, must not
        // bump, and must not create a second tag (§2 idempotency; the probe
        // peels the annotated tag to compare commits, not tag objects).
        let rerun = try script(Self.versionScript, ["--tag", "--push"], env: dateEnv)
        XCTAssertEqual(rerun.status, 0, rerun.stderr)
        XCTAssertTrue(rerun.stdout.contains("version=2026.7.1"), rerun.stdout)
        XCTAssertTrue(rerun.stdout.contains("action=reuse"), rerun.stdout)
    }

    func testMintNeverAutoBumpsAPinnedVersion() throws {
        let dateEnv = ["VERSION_DATE_OVERRIDE": "2026-07-02"]
        try commit("june work", date: "2026-06-15T12:00:00Z")
        let first = try script(Self.versionScript, ["--tag", "--push"], env: dateEnv)
        XCTAssertEqual(first.status, 0, first.stderr) // v2026.7.1 now taken

        try commit("new commit", date: "2026-07-02T08:00:00Z")
        var env = dateEnv
        env["MARKETING_VERSION"] = "2026.7.1"
        let pinned = try script(Self.versionScript, ["--tag"], env: env)
        XCTAssertNotEqual(pinned.status, 0, "a pin must fail loudly when its tag exists on a different commit")
        XCTAssertTrue(pinned.stderr.contains("never auto-bumped"), pinned.stderr)
    }

    func testMintFailsClosedWhenRemoteProbeFails() throws {
        try commit("work", date: "2026-07-02T08:00:00Z")
        let missing = fixtureDir.appendingPathComponent("missing.git").path
        try git(["remote", "set-url", "origin", missing], cwd: workDir)
        let result = try script(Self.versionScript, ["--tag"], env: ["VERSION_DATE_OVERRIDE": "2026-07-02"])
        XCTAssertNotEqual(result.status, 0, "a failed remote probe must never mint blind")
        XCTAssertTrue(result.stderr.contains("refusing to mint blind"), result.stderr)
    }

    // MARK: - Build number: total count, --at ref, pin, fail-closed

    func testBuildNumberIsTotalCommitCountAndSupportsAtRef() throws {
        try commit("one", date: "2026-06-05T09:00:00Z")
        try commit("two", date: "2026-06-06T09:00:00Z")
        try commit("three", date: "2026-07-02T09:00:00Z")

        let head = try script(Self.buildNumberScript)
        XCTAssertEqual(head.stdout.trimmed, "3", "build number is the TOTAL count — it never resets with the month")

        let atRef = try script(Self.buildNumberScript, ["--at", "HEAD~1"])
        XCTAssertEqual(atRef.stdout.trimmed, "2")
    }

    func testBuildNumberPinIsEmittedVerbatim() throws {
        try commit("one", date: "2026-06-05T09:00:00Z")
        let result = try script(Self.buildNumberScript, env: ["BUILD_NUMBER": "4242"])
        XCTAssertEqual(result.stdout.trimmed, "4242")
    }

    func testBuildNumberFailsClosedOnUnresolvableRef() throws {
        try commit("one", date: "2026-06-05T09:00:00Z")
        let result = try script(Self.buildNumberScript, ["--at", "no-such-ref"])
        XCTAssertNotEqual(result.status, 0)
        XCTAssertTrue(result.stdout.isEmpty, "fail-closed means NO number on stdout (never a clock value)")
    }

    // MARK: - §6 migration: semver → CalVer needs no cutover gate

    /// devcanopy's last semver tag was v0.1.1. Any CalVer (2026.M.P) strictly
    /// exceeds it under numeric component ordering, so the switch is
    /// monotonic-safe mid-month with no gate (spec §6).
    func testCalVerStrictlyExceedsLastShippedSemver() throws {
        try commit("work", date: "2026-06-15T12:00:00Z")
        let result = try script(Self.versionScript, ["--version"], env: ["VERSION_DATE_OVERRIDE": "2026-07-10"])
        let calver = result.stdout.trimmed.split(separator: ".").compactMap { Int($0) }
        let legacy = [0, 1, 1] // v0.1.1, the last semver release
        XCTAssertEqual(calver.count, 3, "expected YYYY.M.P, got: \(result.stdout)")
        XCTAssertTrue(isVersionOrderedAbove(calver, legacy), "\(calver) must order above \(legacy)")
    }

    private func isVersionOrderedAbove(_ lhs: [Int], _ rhs: [Int]) -> Bool {
        for (left, right) in zip(lhs, rhs) where left != right {
            return left > right
        }
        return lhs.count > rhs.count
    }
}

private extension String {
    var trimmed: String {
        trimmingCharacters(in: .whitespacesAndNewlines)
    }
}
