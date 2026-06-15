import Foundation

/// Static container/VM lists for demo mode, so the Containers panel has plausible
/// content without a runtime or remote agent. Off-path for normal launches.
enum DemoContainers {
    /// Containers reported by the synthetic remote build host.
    static let remote: [ContainerInfo] = [
        ContainerInfo(
            name: "ci-runner-1",
            statusText: "Up 4 hours",
            isRunning: true,
            runtime: .docker,
            image: "ghcr.io/actions/runner:latest"
        ),
        ContainerInfo(
            name: "postgres-dev",
            statusText: "Up 2 days",
            isRunning: true,
            runtime: .docker,
            image: "postgres:16"
        ),
        ContainerInfo(
            name: "redis",
            statusText: "Up 2 days",
            isRunning: true,
            runtime: .podman,
            image: "redis:7"
        ),
        ContainerInfo(
            name: "build-vm",
            statusText: "running",
            isRunning: true,
            runtime: .tart,
            image: nil
        ),
        ContainerInfo(
            name: "old-staging",
            statusText: "Exited (0) 6 hours ago",
            isRunning: false,
            runtime: .docker,
            image: "node:20"
        )
    ]
}
