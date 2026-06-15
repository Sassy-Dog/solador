import XCTest
@testable import DevCanopy

final class GitStatusParserTests: XCTestCase {

    // MARK: - Clean / synced

    func testSyncedCleanRepo() {
        let output = """
        # branch.oid abc123abc123abc123abc123abc123abc123abcd
        # branch.head main
        # branch.upstream origin/main
        # branch.ab +0 -0
        """
        let status = GitStatusParser.parseStatus(from: output, revListOutput: nil)

        XCTAssertEqual(status.branch, "main")
        XCTAssertEqual(status.trackingBranch, "origin/main")
        XCTAssertFalse(status.hasUncommittedChanges)
        XCTAssertEqual(status.aheadCount, 0)
        XCTAssertEqual(status.behindCount, 0)
        XCTAssertFalse(status.isDetached)
    }

    // MARK: - Ahead / behind / diverged (branch.ab sign handling)

    func testAheadOnly() {
        let output = """
        # branch.head main
        # branch.upstream origin/main
        # branch.ab +3 -0
        """
        let status = GitStatusParser.parseStatus(from: output, revListOutput: nil)
        XCTAssertEqual(status.aheadCount, 3)
        XCTAssertEqual(status.behindCount, 0)
    }

    func testBehindOnly() {
        // Behind is reported as a negative number in porcelain v2; parser must
        // strip the sign and store a positive magnitude.
        let output = """
        # branch.head main
        # branch.upstream origin/main
        # branch.ab +0 -5
        """
        let status = GitStatusParser.parseStatus(from: output, revListOutput: nil)
        XCTAssertEqual(status.aheadCount, 0)
        XCTAssertEqual(status.behindCount, 5)
    }

    func testDiverged() {
        let output = """
        # branch.head feature
        # branch.upstream origin/feature
        # branch.ab +2 -4
        """
        let status = GitStatusParser.parseStatus(from: output, revListOutput: nil)
        XCTAssertEqual(status.aheadCount, 2)
        XCTAssertEqual(status.behindCount, 4)
    }

    // MARK: - Detached HEAD

    func testDetachedHead() {
        let output = """
        # branch.oid abc123abc123abc123abc123abc123abc123abcd
        # branch.head (detached)
        """
        let status = GitStatusParser.parseStatus(from: output, revListOutput: nil)
        XCTAssertTrue(status.isDetached)
        XCTAssertEqual(status.branch, "HEAD")
        XCTAssertNil(status.trackingBranch)
    }

    // MARK: - Uncommitted changes

    func testUntrackedFilesOnly() {
        let output = """
        # branch.head main
        # branch.upstream origin/main
        # branch.ab +0 -0
        ? newfile.txt
        ? other.swift
        """
        let status = GitStatusParser.parseStatus(from: output, revListOutput: nil)
        XCTAssertTrue(status.hasUncommittedChanges)
    }

    func testTrackedFileChanges() {
        let output = """
        # branch.head main
        1 .M N... 100644 100644 100644 abc def file.swift
        """
        let status = GitStatusParser.parseStatus(from: output, revListOutput: nil)
        XCTAssertTrue(status.hasUncommittedChanges)
    }

    func testRenamedFileChanges() {
        let output = """
        # branch.head main
        2 R. N... 100644 100644 100644 abc def R100 new.swift\told.swift
        """
        let status = GitStatusParser.parseStatus(from: output, revListOutput: nil)
        XCTAssertTrue(status.hasUncommittedChanges)
    }

    func testUnmergedFiles() {
        let output = """
        # branch.head main
        u UU N... 100644 100644 100644 100644 a b c d conflict.swift
        """
        let status = GitStatusParser.parseStatus(from: output, revListOutput: nil)
        XCTAssertTrue(status.hasUncommittedChanges)
    }

    // MARK: - Missing upstream

    func testMissingUpstream() {
        let output = """
        # branch.oid abc123abc123abc123abc123abc123abc123abcd
        # branch.head feature-no-remote
        """
        let status = GitStatusParser.parseStatus(from: output, revListOutput: nil)
        XCTAssertEqual(status.branch, "feature-no-remote")
        XCTAssertNil(status.trackingBranch)
        XCTAssertEqual(status.aheadCount, 0)
        XCTAssertEqual(status.behindCount, 0)
        XCTAssertFalse(status.isDetached)
    }

    // MARK: - rev-list override

    func testRevListOverridesAheadBehind() {
        // status reports +1 -1, but the authoritative rev-list output wins.
        let output = """
        # branch.head main
        # branch.upstream origin/main
        # branch.ab +1 -1
        """
        let status = GitStatusParser.parseStatus(from: output, revListOutput: "7\t3")
        XCTAssertEqual(status.aheadCount, 7)
        XCTAssertEqual(status.behindCount, 3)
    }

    func testMalformedRevListFallsBackToStatus() {
        // A rev-list output without two tab-separated fields must not clobber
        // the values already parsed from status.
        let output = """
        # branch.head main
        # branch.upstream origin/main
        # branch.ab +2 -4
        """
        let status = GitStatusParser.parseStatus(from: output, revListOutput: "garbage")
        XCTAssertEqual(status.aheadCount, 2)
        XCTAssertEqual(status.behindCount, 4)
    }

    func testRevListWithNonNumericFieldsYieldsZero() {
        let output = """
        # branch.head main
        # branch.upstream origin/main
        # branch.ab +2 -4
        """
        let status = GitStatusParser.parseStatus(from: output, revListOutput: "abc\tdef")
        XCTAssertEqual(status.aheadCount, 0)
        XCTAssertEqual(status.behindCount, 0)
    }

    // MARK: - Remote branches

    func testParseRemoteBranches() {
        let output = """
          origin/main
          origin/feature-x
          some/local-noise
          origin/release-1.0
        """
        let branches = GitStatusParser.parseRemoteBranches(from: output)
        XCTAssertEqual(branches, ["origin/main", "origin/feature-x", "origin/release-1.0"])
    }

    func testParseRemoteBranchesEmpty() {
        XCTAssertTrue(GitStatusParser.parseRemoteBranches(from: "").isEmpty)
    }
}
