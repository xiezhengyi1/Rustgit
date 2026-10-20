// swift-tools-version:6.0
import PackageDescription

let package = Package(
    name: "RgitClient",
    platforms: [.macOS(.v14)],
    products: [
        .library(name: "RgitClient", targets: ["RgitClient"]),
    ],
    targets: [
        .target(name: "RgitClient"),
        // Swift Testing only: XCTest is not part of the Command Line Tools.
        .testTarget(name: "RgitClientTests", dependencies: ["RgitClient"]),
    ]
)
