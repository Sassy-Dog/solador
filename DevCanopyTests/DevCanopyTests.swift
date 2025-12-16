import XCTest
@testable import DevCanopy

final class DevCanopyTests: XCTestCase {
    
    func testRepositoryStatusSynced() {
        let repo = TrackedRepository(name: "test-repo", path: "/tmp/test")
        repo.aheadCount = 0
        repo.behindCount = 0
        repo.hasUncommittedChanges = false
        
        XCTAssertEqual(repo.displayStatus, .synced)
    }
    
    func testRepositoryStatusAhead() {
        let repo = TrackedRepository(name: "test-repo", path: "/tmp/test")
        repo.aheadCount = 2
        repo.behindCount = 0
        repo.hasUncommittedChanges = false
        
        XCTAssertEqual(repo.displayStatus, .ahead)
    }
    
    func testRepositoryStatusBehind() {
        let repo = TrackedRepository(name: "test-repo", path: "/tmp/test")
        repo.aheadCount = 0
        repo.behindCount = 3
        repo.hasUncommittedChanges = false
        
        XCTAssertEqual(repo.displayStatus, .behind)
    }
    
    func testRepositoryStatusDiverged() {
        let repo = TrackedRepository(name: "test-repo", path: "/tmp/test")
        repo.aheadCount = 1
        repo.behindCount = 2
        repo.hasUncommittedChanges = false
        
        XCTAssertEqual(repo.displayStatus, .diverged)
    }
    
    func testRepositoryStatusUncommitted() {
        let repo = TrackedRepository(name: "test-repo", path: "/tmp/test")
        repo.aheadCount = 0
        repo.behindCount = 0
        repo.hasUncommittedChanges = true
        
        XCTAssertEqual(repo.displayStatus, .uncommitted)
    }
    
    func testRefreshIntervals() {
        XCTAssertEqual(RefreshInterval.thirtySeconds.rawValue, 30)
        XCTAssertEqual(RefreshInterval.oneMinute.rawValue, 60)
        XCTAssertEqual(RefreshInterval.fiveMinutes.rawValue, 300)
    }
    
    func testServiceTypes() {
        XCTAssertEqual(ServiceType.allCases.count, 4)
        XCTAssertTrue(ServiceType.allCases.contains(.github))
        XCTAssertTrue(ServiceType.allCases.contains(.vercel))
        XCTAssertTrue(ServiceType.allCases.contains(.neon))
        XCTAssertTrue(ServiceType.allCases.contains(.cloudflare))
    }
}