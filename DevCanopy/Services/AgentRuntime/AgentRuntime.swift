import Combine
import Foundation

/// A monitored agent runtime. OpenClaw is the first conformer; hermes is the
/// intended second. The coordinator and panel depend on this protocol and on
/// `AgentRuntimeSnapshot` — never on a concrete client — so a new runtime is
/// added by writing a conformer, not by touching the panel or domain layer.
///
/// Deliberately minimal: lifecycle plus a published snapshot. No RPC/command
/// surface leaks across the boundary, which keeps the read-only contract honest.
@MainActor
protocol AgentRuntime: AnyObject {
    /// The current rollup for this runtime. Conformers mark it `@Published`.
    var snapshot: AgentRuntimeSnapshot { get }
    /// Combine publisher so the coordinator can observe snapshot changes without
    /// depending on each conformer's concrete `ObservableObject` type.
    var snapshotPublisher: AnyPublisher<AgentRuntimeSnapshot, Never> { get }

    func start()
    func stop()
}
