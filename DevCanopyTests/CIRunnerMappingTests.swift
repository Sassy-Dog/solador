@testable import DevCanopy
import XCTest

final class CIRunnerMappingTests: XCTestCase {
    private func dto(_ name: String, os: String, status: String, busy: Bool) -> RunnerDTO {
        RunnerDTO(id: abs(name.hashValue), name: name, os: os, status: status, busy: busy, labels: nil)
    }

    func testStateDerivation() {
        let runners = CIRunnerMapping.map(dtos: [
            dto("a", os: "macOS", status: "online", busy: true),
            dto("b", os: "Linux", status: "online", busy: false),
            dto("c", os: "Linux", status: "offline", busy: false)
        ])
        XCTAssertEqual(runners[0].state, .busy)
        XCTAssertEqual(runners[1].state, .idle)
        XCTAssertEqual(runners[2].state, .offline) // offline regardless of busy
    }

    func testOSClassification() {
        let runners = CIRunnerMapping.map(dtos: [
            dto("a", os: "macOS", status: "online", busy: false),
            dto("b", os: "Linux", status: "online", busy: false),
            dto("c", os: "Windows", status: "online", busy: false)
        ])
        XCTAssertEqual(runners[0].os, .macOS)
        XCTAssertEqual(runners[1].os, .linux)
        XCTAssertEqual(runners[2].os, .other)
    }

    func testSummaryCounts() {
        let summary = CIRunnerMapping.summarize(CIRunnerMapping.map(dtos: [
            dto("m1", os: "macOS", status: "online", busy: true),
            dto("m2", os: "macOS", status: "online", busy: false),
            dto("l1", os: "Linux", status: "online", busy: false),
            dto("l2", os: "Linux", status: "offline", busy: false)
        ]))
        XCTAssertEqual(summary.total, 4)
        XCTAssertEqual(summary.online, 3)
        XCTAssertEqual(summary.busy, 1)
        XCTAssertEqual(summary.idle, 2)
        XCTAssertEqual(summary.macOSOnline, 2)
        XCTAssertEqual(summary.macOSTotal, 2)
        XCTAssertEqual(summary.linuxOnline, 1)
        XCTAssertEqual(summary.linuxTotal, 2)
    }
}

final class PortfolioReposTests: XCTestCase {
    private let slugs = PortfolioRepos.seedSlugs

    func testNormalizedMatchingHandlesPunctuation() {
        XCTAssertTrue(PortfolioRepos.matches(repoDirName: "tailoredtip", in: slugs)) // slug tailored-tip
        XCTAssertTrue(PortfolioRepos.matches(repoDirName: "qr-ninja", in: slugs))
        XCTAssertTrue(PortfolioRepos.matches(repoDirName: "velovate", in: slugs))
        XCTAssertTrue(PortfolioRepos.matches(repoDirName: "what2wear", in: slugs))
        XCTAssertTrue(PortfolioRepos.matches(repoDirName: "devcanopy", in: slugs)) // now tracked
        XCTAssertTrue(PortfolioRepos.matches(repoDirName: "platform", in: slugs)) // now tracked
        XCTAssertFalse(PortfolioRepos.matches(repoDirName: "blog", in: slugs))
    }

    func testMatchingRespectsTheProvidedSet() {
        // A repo only matches when its slug is in the live set.
        XCTAssertTrue(PortfolioRepos.matches(repoDirName: "velovate", in: ["Sassy-Dog/velovate"]))
        XCTAssertFalse(PortfolioRepos.matches(repoDirName: "velovate", in: ["Sassy-Dog/qr-ninja"]))
    }
}
