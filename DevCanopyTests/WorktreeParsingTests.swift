@testable import DevCanopy
import XCTest

final class WorktreeParsingTests: XCTestCase {
    func testParsesMultipleRecordsWithBranches() {
        let output = """
        worktree /Users/dev/Repos/foo
        HEAD abc123abc123abc123abc123abc123abc123abcd
        branch refs/heads/main

        worktree /Users/dev/Repos/foo/.claude/worktrees/agent-x
        HEAD def456def456def456def456def456def456def4
        branch refs/heads/feature-y

        """
        let worktrees = WorktreeParsing.parseWorktreeList(output)

        XCTAssertEqual(worktrees.count, 2)

        XCTAssertEqual(worktrees[0].path, "/Users/dev/Repos/foo")
        XCTAssertEqual(worktrees[0].head, "abc123abc123abc123abc123abc123abc123abcd")
        XCTAssertEqual(worktrees[0].branch, "main")
        XCTAssertFalse(worktrees[0].isDetached)
        XCTAssertFalse(worktrees[0].isBare)

        XCTAssertEqual(worktrees[1].path, "/Users/dev/Repos/foo/.claude/worktrees/agent-x")
        XCTAssertEqual(worktrees[1].branch, "feature-y")
    }

    func testParsesDetachedWorktree() {
        let output = """
        worktree /Users/dev/Repos/bar
        HEAD 0123456789012345678901234567890123456789
        detached

        """
        let worktrees = WorktreeParsing.parseWorktreeList(output)
        XCTAssertEqual(worktrees.count, 1)
        XCTAssertTrue(worktrees[0].isDetached)
        XCTAssertNil(worktrees[0].branch)
        XCTAssertEqual(worktrees[0].head, "0123456789012345678901234567890123456789")
    }

    func testParsesBareWorktree() {
        let output = """
        worktree /Users/dev/Repos/baz.git
        bare

        worktree /Users/dev/Repos/baz/main
        HEAD aaaa111122223333444455556666777788889999
        branch refs/heads/main

        """
        let worktrees = WorktreeParsing.parseWorktreeList(output)
        XCTAssertEqual(worktrees.count, 2)
        XCTAssertTrue(worktrees[0].isBare)
        XCTAssertEqual(worktrees[0].path, "/Users/dev/Repos/baz.git")
        XCTAssertFalse(worktrees[1].isBare)
        XCTAssertEqual(worktrees[1].branch, "main")
    }

    func testEmptyYieldsNothing() {
        XCTAssertTrue(WorktreeParsing.parseWorktreeList("").isEmpty)
        XCTAssertTrue(WorktreeParsing.parseWorktreeList("\n\n").isEmpty)
    }
}
