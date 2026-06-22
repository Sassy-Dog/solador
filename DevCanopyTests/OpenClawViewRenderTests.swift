import Combine
@testable import DevCanopy
import SwiftUI
import XCTest

/// Renders the OpenClaw views across their states with `ImageRenderer` so the
/// view bodies (and the custom channel-flow layout) actually execute. This is
/// smoke coverage — it asserts rendering doesn't trap — across every branch the
/// panel can show: idle, connecting, disconnected, pairing, and a fully
/// populated multi-runtime rollup.
@MainActor
final class OpenClawViewRenderTests: XCTestCase {
    /// Minimal in-memory `AgentRuntime` that just republishes a fixed snapshot —
    /// lets us drive the panel into any state without a socket.
    private final class StubRuntime: ObservableObject, AgentRuntime {
        @Published var snapshot: AgentRuntimeSnapshot
        init(_ snapshot: AgentRuntimeSnapshot) {
            self.snapshot = snapshot
        }

        var snapshotPublisher: AnyPublisher<AgentRuntimeSnapshot, Never> {
            $snapshot.eraseToAnyPublisher()
        }

        func start() {}
        func stop() {}
    }

    private func render(_ view: some View) {
        let renderer = ImageRenderer(content: view.frame(width: 400, height: 300))
        _ = renderer.nsImage // forces body evaluation + layout
    }

    private func coordinator(_ snapshots: AgentRuntimeSnapshot...) -> AgentRuntimeCoordinator {
        AgentRuntimeCoordinator(runtimes: snapshots.map { StubRuntime($0) })
    }

    private func populated(id: String = "openclaw", name: String = "OpenClaw") -> AgentRuntimeSnapshot {
        AgentRuntimeSnapshot(
            id: id,
            displayName: name,
            connection: .connected,
            agents: [
                AgentRollupItem(id: "main", name: "Sebastian", emoji: "🦀", status: .running, detail: "opus-4-8"),
                AgentRollupItem(id: "helper", name: "Helper", status: .ok, detail: "sonnet")
            ],
            cron: CronSummary(
                ok: 2,
                running: 1,
                error: 1,
                lastError: "disk full",
                jobs: [AgentRollupItem(id: "n", name: "n", status: .error, detail: "disk full")]
            ),
            channels: [
                ChannelStatus(id: "slack", name: "slack", status: .ok, lastError: nil),
                ChannelStatus(id: "telegram", name: "telegram", status: .unknown, lastError: nil),
                ChannelStatus(id: "whatsapp", name: "whatsapp", status: .disabled, lastError: nil)
            ],
            usage: SessionUsageRollup(
                totalTokens: 1_234_567,
                contextTokens: 5000,
                inputTokens: 1,
                outputTokens: 2,
                updatedAt: Date(timeIntervalSince1970: 1)
            ),
            pairing: nil,
            lastUpdated: Date(timeIntervalSince1970: 1)
        )
    }

    func testRendersPopulatedSingleRuntime() {
        render(OpenClawPanel().environmentObject(coordinator(populated())))
    }

    func testRendersMultiRuntimeSubHeaders() {
        render(OpenClawPanel().environmentObject(coordinator(populated(), populated(id: "hermes", name: "Hermes"))))
    }

    func testRendersPairingBanner() {
        var snap = AgentRuntimeSnapshot.idle(id: "openclaw", displayName: "OpenClaw")
        snap.connection = .disconnected(reason: "awaiting device pairing")
        snap.pairing = PairingState(
            deviceID: "abc123def456",
            requestID: "req-1",
            kind: .firstPair,
            remediationHint: "approve on the gateway"
        )
        render(OpenClawPanel().environmentObject(coordinator(snap)))
    }

    func testRendersScopeUpgradeBanner() {
        var snap = AgentRuntimeSnapshot.idle(id: "openclaw", displayName: "OpenClaw")
        snap.pairing = PairingState(deviceID: "dev", requestID: "r", kind: .scopeUpgrade, remediationHint: nil)
        render(OpenClawPanel().environmentObject(coordinator(snap)))
    }

    func testRendersDisconnectedAndConnectingAndIdle() {
        var disconnected = AgentRuntimeSnapshot.idle(id: "o", displayName: "OpenClaw")
        disconnected.connection = .disconnected(reason: "gateway rejected: nope")
        render(OpenClawPanel().environmentObject(coordinator(disconnected)))

        var connecting = AgentRuntimeSnapshot.idle(id: "o", displayName: "OpenClaw")
        connecting.connection = .connecting
        render(OpenClawPanel().environmentObject(coordinator(connecting)))

        render(OpenClawPanel().environmentObject(coordinator(.idle(id: "o", displayName: "OpenClaw"))))
    }

    func testRendersEmptyCoordinator() {
        render(OpenClawPanel().environmentObject(AgentRuntimeCoordinator(runtimes: [])))
    }

    func testRendersSettingsView() {
        let service = OpenClawService(urlProvider: { "ws://x:1" }, tokenProvider: { nil })
        render(OpenClawSettingsView().environmentObject(service))
    }
}
