@testable import DevCanopy
import XCTest

final class AzureCostCSVTests: XCTestCase {
    // MARK: - monthRangeFolder

    func testMonthRangeFolderNamesTheCalendarMonthFolder() {
        XCTAssertEqual(monthRangeFolder(utc(2026, 6, 15, 12)), "20260601-20260630")
        // 28-day February, and zero-padding of single-digit months/days.
        XCTAssertEqual(monthRangeFolder(utc(2027, 2, 3)), "20270201-20270228")
        // Uses UTC, not local time — an end-of-month UTC instant stays in that month.
        XCTAssertEqual(monthRangeFolder(utc(2026, 12, 31, 23, 30)), "20261201-20261231")
    }

    func testPriorMonthDateRollsBackOneCalendarMonth() {
        XCTAssertEqual(monthRangeFolder(priorMonthDate(from: utc(2026, 6, 15))), "20260501-20260531")
        // January rolls back into the previous December.
        XCTAssertEqual(monthRangeFolder(priorMonthDate(from: utc(2026, 1, 10))), "20251201-20251231")
    }

    // MARK: - parseCsv (RFC4180)

    func testParseCsvKeepsCommasAndEscapedQuotesInsideQuotedFields() {
        // The export's `tags` column embeds quoted JSON with commas — a naive split
        // would shift every later column. "" is an escaped quote.
        let rows = parseCsv("a,b,c\n1,\"x,y\",\"he said \"\"hi\"\"\"\n")
        XCTAssertEqual(rows[0], ["a", "b", "c"])
        XCTAssertEqual(rows[1], ["1", "x,y", "he said \"hi\""])
    }

    func testParseCsvStripsLeadingUTF8BOMFromHeader() {
        let rows = parseCsv("\u{FEFF}col\nval\n")
        XCTAssertEqual(rows[0], ["col"])
    }

    func testParseCsvSplitsRowsOnCRLFLineEndings() {
        // The real Azure export uses CRLF. Swift folds "\r\n" into a single
        // grapheme-cluster Character, so a Character-based scan collapses the whole
        // file into one row — this guards the scalar-based parse (\r and \n apart).
        let rows = parseCsv("a,b\r\n1,2\r\n3,4\r\n")
        XCTAssertEqual(rows, [["a", "b"], ["1", "2"], ["3", "4"]])
    }

    func testAggregateSumsCostAcrossCRLFRows() throws {
        // Mirrors the production failure: a CRLF export summed to $0 because every
        // data row folded into the header row. Scalar parsing splits them correctly.
        let csv = "resourceGroupName,costInUsd\r\nrg-a,10\r\nrg-b,5\r\n"
        let (total, byResource) = try aggregateCostCsv(csv)
        XCTAssertEqual(total, 15, accuracy: 1e-6)
        assertResources(byResource, [("rg-a", 10), ("rg-b", 5)])
    }

    // MARK: - aggregateCostCsv

    func testAggregateSumsCostInUsdAndGroupsByResourceGroupDesc() throws {
        // A miniature export CSV with the columns the dashboard reads plus a tags
        // column carrying an embedded comma, to prove the cost column is still found
        // by name after the quoted field.
        let csv = [
            "date,resourceGroupName,costInUsd,tags",
            "2026-06-01,rg-a,12.5,\"{\"\"env\"\":\"\"prod\"\",\"\"team\"\":\"\"x\"\"}\"",
            "2026-06-02,rg-b,7.5,",
            "2026-06-03,,3,", // no resource group → folds into (unassigned)
            "2026-06-04,rg-a,2.5,",
            "" // trailing blank line
        ].joined(separator: "\n")

        let (total, byResource) = try aggregateCostCsv(csv)
        XCTAssertEqual(total, 25.5, accuracy: 1e-6)
        assertResources(byResource, [("rg-a", 15), ("rg-b", 7.5), ("(unassigned)", 3)])
    }

    func testAggregateFoldsCaseVariantResourceGroupsIntoOne() throws {
        let csv = [
            "resourceGroupName,costInUsd",
            "rg-sassydog,175.45",
            "RG-SASSYDOG,22.12", // same group, reported uppercase in some rows
            "rg-velovate-prd,321.65"
        ].joined(separator: "\n")

        let (_, byResource) = try aggregateCostCsv(csv)
        assertResources(byResource, [("rg-velovate-prd", 321.65), ("rg-sassydog", 197.57)])
    }

    func testAggregateNormalizesDisplayCasingToLowercase() throws {
        let (_, byResource) = try aggregateCostCsv("resourceGroupName,costInUsd\nRG-PACKER-BUILD,0.03")
        assertResources(byResource, [("rg-packer-build", 0.03)])
    }

    func testAggregateThrowsWhenCostColumnAbsent() {
        XCTAssertThrowsError(try aggregateCostCsv("date,resourceGroupName\n2026-06-01,rg-a")) { error in
            XCTAssertEqual(error as? AzureCostError, .missingColumn("costInUsd"))
        }
    }

    func testAggregateHandlesEmptyExportGracefully() throws {
        let (total, byResource) = try aggregateCostCsv("")
        XCTAssertEqual(total, 0)
        XCTAssertTrue(byResource.isEmpty)
    }

    // MARK: - parseBlobListXML

    func testParseBlobListXMLExtractsNamesAndNilMarker() {
        let xml = "<EnumerationResults><Blobs>" +
            "<Blob><Name>daily/x/000001.csv</Name></Blob>" +
            "<Blob><Name>daily/x/000002.csv</Name></Blob>" +
            "</Blobs><NextMarker></NextMarker></EnumerationResults>"
        let parsed = parseBlobListXML(xml)
        XCTAssertEqual(parsed.names, ["daily/x/000001.csv", "daily/x/000002.csv"])
        XCTAssertNil(parsed.nextMarker)
    }

    func testParseBlobListXMLReturnsNonEmptyMarker() {
        let parsed = parseBlobListXML("<NextMarker>page2</NextMarker>")
        XCTAssertEqual(parsed.nextMarker, "page2")
    }

    // MARK: - Reader (protocol seam, no network)

    func testReadExportPicksLatestRunAndSumsPartitions() async throws {
        let prefix = "\(AzureCostReader.mtdPrefix)/20260601-20260630/"
        let stub = StubBlobFetcher(blobs: [
            // Older run — must be ignored.
            "\(prefix)202606150900/g1/000001.csv": "resourceGroupName,costInUsd\nrg-a,1",
            // Newest run, split across two partitions (case-variant RG across them).
            "\(prefix)202606151800/g2/000001.csv": "resourceGroupName,costInUsd\nrg-a,10\nrg-b,5",
            "\(prefix)202606151800/g2/000002.csv": "resourceGroupName,costInUsd\nRG-A,2",
            // Manifest in the newest run — not a .csv, must be skipped.
            "\(prefix)202606151800/g2/_manifest.json": "{}"
        ])

        let result = try await AzureCostReader.readExport(
            fetcher: stub,
            rootPrefix: AzureCostReader.mtdPrefix,
            month: utc(2026, 6, 15)
        )
        XCTAssertEqual(result.total, 17, accuracy: 1e-6)
        assertResources(result.byResource, [("rg-a", 12), ("rg-b", 5)])
    }

    func testFetchSummaryCombinesMTDAndPriorMonth() async throws {
        let mtd = "\(AzureCostReader.mtdPrefix)/20260601-20260630/202606151800/g/000001.csv"
        let prior = "\(AzureCostReader.priorPrefix)/20260501-20260531/202606010300/g/000001.csv"
        let stub = StubBlobFetcher(blobs: [
            mtd: "resourceGroupName,costInUsd\nrg-a,10\nrg-b,5",
            prior: "resourceGroupName,costInUsd\nrg-a,100"
        ])

        let summary = try await AzureCostReader.fetchSummary(fetcher: stub, now: utc(2026, 6, 15))
        XCTAssertEqual(summary.spendMTD, 15, accuracy: 1e-6)
        XCTAssertEqual(summary.spendPriorMonth, 100, accuracy: 1e-6)
        XCTAssertNil(summary.error)
        assertResources(summary.byResource, [("rg-a", 10), ("rg-b", 5)])
    }

    func testFetchSummaryIsBestEffortAboutPriorMonth() async throws {
        // Only the MTD export exists; the prior-month read fails and must not blank
        // the card — prior folds to 0, error stays nil.
        let mtd = "\(AzureCostReader.mtdPrefix)/20260601-20260630/202606151800/g/000001.csv"
        let stub = StubBlobFetcher(blobs: [mtd: "resourceGroupName,costInUsd\nrg-a,42"])

        let summary = try await AzureCostReader.fetchSummary(fetcher: stub, now: utc(2026, 6, 15))
        XCTAssertEqual(summary.spendMTD, 42, accuracy: 1e-6)
        XCTAssertEqual(summary.spendPriorMonth, 0)
        XCTAssertNil(summary.error)
    }

    // MARK: - Helpers

    private func utc(_ year: Int, _ month: Int, _ day: Int, _ hour: Int = 0, _ minute: Int = 0) -> Date {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC")!
        return calendar.date(from: DateComponents(
            year: year, month: month, day: day, hour: hour, minute: minute
        ))!
    }

    private func assertResources(
        _ actual: [AzureResourceCost],
        _ expected: [(String, Double)],
        file: StaticString = #filePath,
        line: UInt = #line
    ) {
        XCTAssertEqual(actual.map(\.name), expected.map(\.0), "resource-group order/names", file: file, line: line)
        guard actual.count == expected.count else { return }
        for (resource, want) in zip(actual, expected) {
            XCTAssertEqual(resource.cost, want.1, accuracy: 1e-6, "cost for \(resource.name)", file: file, line: line)
        }
    }
}

/// In-memory `BlobFetching` for the reader tests: `listBlobs` returns the keys that
/// start with the prefix, `getBlobText` returns the stored body. No network.
private struct StubBlobFetcher: BlobFetching {
    let blobs: [String: String]

    func listBlobs(prefix: String) async throws -> [String] {
        blobs.keys.filter { $0.hasPrefix(prefix) }.sorted()
    }

    func getBlobText(path: String) async throws -> String {
        blobs[path] ?? ""
    }
}
