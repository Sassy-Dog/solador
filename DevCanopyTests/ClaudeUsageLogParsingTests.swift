import XCTest
@testable import DevCanopy

final class ClaudeUsageLogParsingTests: XCTestCase {

    // MARK: - Project name derivation from cwd

    func testProjectNameFromPlainRepoPath() {
        XCTAssertEqual(
            ClaudeUsageLog.projectName(fromCWD: "/Users/chris/Repos/sassy-dog/velovate/velovate-app"),
            "velovate-app"
        )
    }

    func testProjectNameStripsClaudeWorktreeSuffix() {
        XCTAssertEqual(
            ClaudeUsageLog.projectName(
                fromCWD: "/Users/chris/Repos/sassy-dog/velovate/velovate-app/.claude/worktrees/agent-a186a09e"
            ),
            "velovate-app"
        )
    }

    func testProjectNameStripsWarpWorktreeSuffix() {
        // .warp-worktrees style suffix also collapses to repo root last component
        XCTAssertEqual(
            ClaudeUsageLog.projectName(
                fromCWD: "/Users/chris/Repos/sassy-dog/velovate/velovate-app/.warp-worktrees/foo-bar"
            ),
            "velovate-app"
        )
    }

    func testProjectNameHandlesTrailingSlash() {
        XCTAssertEqual(
            ClaudeUsageLog.projectName(fromCWD: "/Users/chris/Repos/qr-ninja/"),
            "qr-ninja"
        )
    }

    func testProjectNameEmptyFallsBackToUnknown() {
        XCTAssertEqual(ClaudeUsageLog.projectName(fromCWD: ""), "unknown")
        XCTAssertEqual(ClaudeUsageLog.projectName(fromCWD: "/"), "unknown")
    }

    // MARK: - Line parsing

    func testParseLineExtractsAssistantUsageRecord() {
        let line = """
        {"type":"assistant","timestamp":"2026-05-29T13:18:22.932Z","requestId":"req_abc","cwd":"/Users/chris/Repos/qr-ninja","message":{"model":"claude-sonnet-4-5","usage":{"input_tokens":6,"output_tokens":475,"cache_creation_input_tokens":0,"cache_read_input_tokens":43049}}}
        """
        let record = ClaudeUsageLog.parseLine(line)
        XCTAssertNotNil(record)
        XCTAssertEqual(record?.requestId, "req_abc")
        XCTAssertEqual(record?.model, "claude-sonnet-4-5")
        XCTAssertEqual(record?.project, "qr-ninja")
        XCTAssertEqual(record?.inputTokens, 6)
        XCTAssertEqual(record?.outputTokens, 475)
        XCTAssertEqual(record?.cacheReadTokens, 43049)
    }

    func testParseLineSkipsNonAssistantLines() {
        let line = #"{"type":"user","timestamp":"2026-05-29T13:18:22.932Z","message":{"role":"user"}}"#
        XCTAssertNil(ClaudeUsageLog.parseLine(line))
    }

    func testParseLineSkipsAssistantWithoutUsage() {
        let line = #"{"type":"assistant","timestamp":"2026-05-29T13:18:22.932Z","requestId":"x","cwd":"/a","message":{"model":"m"}}"#
        XCTAssertNil(ClaudeUsageLog.parseLine(line))
    }

    func testParseLineSkipsMalformed() {
        XCTAssertNil(ClaudeUsageLog.parseLine("not json at all"))
        XCTAssertNil(ClaudeUsageLog.parseLine(""))
    }
}
