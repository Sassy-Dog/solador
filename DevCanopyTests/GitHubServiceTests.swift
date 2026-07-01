@testable import DevCanopy
import XCTest

final class GitHubServiceTests: XCTestCase {
    // MARK: - Link-header branch counting

    /// The branch count is the `page=` of the `rel="last"` URL when paging with
    /// per_page=1: page 37 of 1-per-page == 37 branches.
    func testLastPageParsesRelLast() {
        let header = """
        <https://api.github.com/repositories/1/branches?per_page=1&page=2>; rel="next", \
        <https://api.github.com/repositories/1/branches?per_page=1&page=37>; rel="last"
        """
        XCTAssertEqual(GitHubService.lastPage(fromLinkHeader: header), 37)
    }

    /// A single-page result has no `Link` header — the caller falls back to the
    /// returned array's length, so the parser returns nil here.
    func testLastPageNilWhenHeaderAbsent() {
        XCTAssertNil(GitHubService.lastPage(fromLinkHeader: nil))
    }

    /// A `Link` header without a `rel="last"` relation (e.g. only `first`/`prev`
    /// on the final page) yields nil rather than a wrong count.
    func testLastPageNilWithoutLastRelation() {
        let header = "<https://api.github.com/repositories/1/branches?per_page=1&page=1>; rel=\"first\""
        XCTAssertNil(GitHubService.lastPage(fromLinkHeader: header))
    }

    /// `page` ordering within the URL must not matter — it's parsed by query name.
    func testLastPageHandlesPageBeforePerPage() {
        let header = "<https://api.github.com/x?page=12&per_page=1>; rel=\"last\""
        XCTAssertEqual(GitHubService.lastPage(fromLinkHeader: header), 12)
    }

    // MARK: - Cursor pagination (the /issues undercount bug)

    /// The `/issues` endpoint uses cursor pagination: it emits `rel="next"` with an
    /// opaque `after=` cursor and NO `rel="last"`. `lastPage` must return nil (the
    /// count is unknowable from the header), so the per_page=1 last-page trick can't
    /// be used to count issues.
    func testLastPageNilForCursorPaginatedIssues() {
        let header = "<https://api.github.com/repositories/1/issues?state=open&per_page=1&after=Y3Vyc29yOnYyOpLP&page=2>; rel=\"next\""
        XCTAssertNil(GitHubService.lastPage(fromLinkHeader: header))
    }

    /// `hasNextPage` detects a `rel="next"` so `openCount` can refuse to fall back to
    /// a silent per_page=1 undercount when the total is unknowable (cursor paging).
    func testHasNextPageDetectsNextRelation() {
        let cursor = "<https://api.github.com/repositories/1/issues?per_page=1&after=abc&page=2>; rel=\"next\""
        XCTAssertTrue(GitHubService.hasNextPage(fromLinkHeader: cursor))
        let lastOnly = "<https://api.github.com/x?per_page=1&page=3>; rel=\"last\""
        XCTAssertFalse(GitHubService.hasNextPage(fromLinkHeader: lastOnly))
        XCTAssertFalse(GitHubService.hasNextPage(fromLinkHeader: nil))
    }

    // MARK: - Repo object issue count

    /// Open issue+PR count comes from the repo object's `open_issues_count` (accurate,
    /// core rate limit) — NOT the cursor-paginated `/issues` list. Verify the DTO
    /// decodes GitHub's snake_case field.
    func testRepoDTODecodesOpenIssuesCount() throws {
        let json = Data(#"{"open_issues_count": 2, "name": "platform"}"#.utf8)
        let dto = try JSONDecoder().decode(RepoDTO.self, from: json)
        XCTAssertEqual(dto.openIssuesCount, 2)
    }
}
