// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "HostMetricsKit",
    platforms: [
        .macOS(.v14)
    ],
    products: [
        .library(
            name: "HostMetricsKit",
            targets: ["HostMetricsKit"]
        )
    ],
    targets: [
        .target(
            name: "HostMetricsKit"
        ),
        .testTarget(
            name: "HostMetricsKitTests",
            dependencies: ["HostMetricsKit"]
        )
    ]
)
