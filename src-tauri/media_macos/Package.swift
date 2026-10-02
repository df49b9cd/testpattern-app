// swift-tools-version:5.9
// Companion to src-tauri/src/player/macos.rs: the NSOpenGLView subclass mpv
// renders into (a Rust-only subclass would need objc2's declare_class! +
// drawRect: trampoline, which is the messy part; Swift is native here and the
// result is a tiny static library `libTestpatternSurface.a`).
import PackageDescription
let package = Package(
  name: "TestpatternSurface",
  platforms: [.macOS(.v13)],
  products: [.library(name: "TestpatternSurface", type: .static, targets: ["TestpatternSurface"])],
  targets: [.target(name: "TestpatternSurface", path: "Sources/TestpatternSurface")]
)
