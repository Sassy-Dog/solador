@testable import DevCanopy
import XCTest

final class ContainerParsingTests: XCTestCase {
    // MARK: - docker/podman ps

    func testParsePsOutputParsesPipeDelimitedRunningAndStopped() {
        let output = """
        web|Up 3 hours|nginx:latest
        db|Exited (0) 2 days ago|postgres:16
        cache|Up 5 minutes (healthy)|redis:7
        """
        let containers = ContainerParsing.parsePsOutput(output, runtime: .docker)

        XCTAssertEqual(containers.count, 3)

        XCTAssertEqual(containers[0].name, "web")
        XCTAssertEqual(containers[0].statusText, "Up 3 hours")
        XCTAssertEqual(containers[0].image, "nginx:latest")
        XCTAssertTrue(containers[0].isRunning)
        XCTAssertEqual(containers[0].runtime, .docker)

        XCTAssertEqual(containers[1].name, "db")
        XCTAssertFalse(containers[1].isRunning)
        XCTAssertEqual(containers[1].image, "postgres:16")

        XCTAssertTrue(containers[2].isRunning)
    }

    func testParsePsOutputSkipsBlankLines() {
        let output = "\n\nweb|Up 1 minute|nginx\n\n"
        let containers = ContainerParsing.parsePsOutput(output, runtime: .podman)
        XCTAssertEqual(containers.count, 1)
        XCTAssertEqual(containers[0].name, "web")
        XCTAssertEqual(containers[0].runtime, .podman)
    }

    func testParsePsOutputHandlesMissingImage() {
        let output = "lonely|Up 2 hours|"
        let containers = ContainerParsing.parsePsOutput(output, runtime: .docker)
        XCTAssertEqual(containers.count, 1)
        XCTAssertEqual(containers[0].name, "lonely")
        XCTAssertNil(containers[0].image)
    }

    func testParsePsOutputEmptyYieldsNothing() {
        XCTAssertTrue(ContainerParsing.parsePsOutput("", runtime: .docker).isEmpty)
        XCTAssertTrue(ContainerParsing.parsePsOutput("   \n  ", runtime: .docker).isEmpty)
    }

    // MARK: - tart list

    func testParseTartListParsesRowsAndState() {
        let output = """
        Source Name Disk Size State
        local  ci-runner-1  50  20  running
        local  ci-runner-2  50  18  stopped
        oci    base-sonoma  60  40  running
        """
        let vms = ContainerParsing.parseTartList(output)

        XCTAssertEqual(vms.count, 3)

        XCTAssertEqual(vms[0].name, "ci-runner-1")
        XCTAssertTrue(vms[0].isRunning)
        XCTAssertEqual(vms[0].statusText, "running")
        XCTAssertEqual(vms[0].runtime, .tart)
        XCTAssertNil(vms[0].image)

        XCTAssertEqual(vms[1].name, "ci-runner-2")
        XCTAssertFalse(vms[1].isRunning)

        XCTAssertEqual(vms[2].name, "base-sonoma")
        XCTAssertTrue(vms[2].isRunning)
    }

    func testParseTartListSkipsHeaderAndBlankLines() {
        let output = "Source Name Disk Size State\n\nlocal x 1 1 running\n\n"
        let vms = ContainerParsing.parseTartList(output)
        XCTAssertEqual(vms.count, 1)
        XCTAssertEqual(vms[0].name, "x")
    }

    func testParseTartListEmptyYieldsNothing() {
        XCTAssertTrue(ContainerParsing.parseTartList("").isEmpty)
        XCTAssertTrue(ContainerParsing.parseTartList("Source Name Disk Size State\n").isEmpty)
    }
}
