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
}
