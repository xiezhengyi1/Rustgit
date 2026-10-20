// swift-tools-version:6.0
import PackageDescription

// Build with `swift build -c release` (Command Line Tools are enough; no Xcode project needed).
// scripts/macos/build-app.sh assembles the .app bundle around the produced executable.
let package = Package(
    name: "RustGit",
    platforms: [.macOS(.v14)],
    products: [
        .executable(name: "RustGit", targets: ["RustGit"]),
    ],
    dependencies: [
        .package(path: "../../packages/swift-client"),
    ],
    targets: [
        // All UI and view-models; a library so they can be unit-tested.
        .target(name: "RustGitKit", dependencies: [.product(name: "RgitClient", package: "swift-client")]),
        .executableTarget(name: "RustGit", dependencies: ["RustGitKit"]),
        .testTarget(name: "RustGitKitTests", dependencies: ["RustGitKit"]),
    ],
    swiftLanguageModes: [.v5]
)
