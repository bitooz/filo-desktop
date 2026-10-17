// swift-tools-version: 6.1
// Filo's computer use bridge: the runner starts it and sends it one JSON request per line; it acts on the user's apps
// through the Accessibility layer of kwwk-computer-use-core (vendor/, see its FILO.md). Built into
// Filo.app/Contents/Resources by scripts/app.sh.
import PackageDescription

let package = Package(
    name: "FiloComputerUse",
    platforms: [.macOS(.v14)],
    targets: [
        .target(
            name: "KWWKComputerUseCore",
            path: "vendor/kwwk-computer-use-core/Sources/KWWKComputerUseCore",
            resources: [.process("Resources")],
            linkerSettings: [
                .linkedFramework("AppKit"),
                .linkedFramework("ApplicationServices"),
                .linkedFramework("Metal"),
                .linkedFramework("QuartzCore"),
            ]
        ),
        .executableTarget(
            name: "FiloComputerUse",
            dependencies: ["KWWKComputerUseCore"],
            path: "Sources/FiloComputerUse"
        ),
    ]
)
