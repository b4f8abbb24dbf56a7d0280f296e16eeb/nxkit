# Native iOS shell

The Rust runtime and host use the same workspace crates and Cargo build as macOS.
iOS platform differences live in those crates behind target configuration. There
is no separate adapter workspace, source snapshot, preparation step or build script.

## Libraries

Use Rust 1.99 and the `aarch64-apple-ios` target. Linking iOS code requires a Mac
with Xcode and an iPhoneOS SDK supporting iOS 26.0. From the repository root:

```sh
# Same runtime library as macOS, with the iOS target selected.
cargo build --locked --release -p nxkit-rt --target aarch64-apple-ios

# Host library, including the C ABI consumed by the Swift shell.
IPHONEOS_DEPLOYMENT_TARGET=26.0 cargo rustc --locked --release -p nxkit-host --lib --target aarch64-apple-ios -- --print native-static-libs
```

The host output is `target/aarch64-apple-ios/release/libnxkit_host.a`. Its header
is `crates/nxkit-host/include/nxkit_ios.h`. The existing `nx2ipa_*` C symbol names
remain stable. `nxkit-rt` still produces its normal static, dynamic and Rust
libraries. No game files or credentials are needed to build these libraries.

## App shell

A Rust library is not an iOS app. Compile `shell/overlay/Gamepad.m` with ARC and
modules, and link it and `libnxkit_host.a` with all five shell Swift sources:
`App.swift`, `StartupDiagnostics.swift`, `MetalView.swift`, `TouchState.swift`,
and `GameMenu.swift`. Use Swift 6, the iPhoneOS SDK, the target
`arm64-apple-ios26.0`, and `shell/BridgingHeader.h` as the Objective-C bridging
header. Pass the native static libraries reported by Cargo to the final linker,
along with UIKit, QuartzCore, CoreText and GameController. Do not link AppKit.

The resulting app must have executable `nxkit` at its root with this directory's
`Info.plist`. Include the root `LICENSE` and `THIRD_PARTY.md` in its
`ThirdPartyNotices` directory. Keep the complete corresponding source and
applicable dependency licenses with redistribution.

The native producer consumes an already built unsigned app, just as the Mac
producer consumes an already built host. It does not build the shell:

```sh
cargo build --locked --release -p nxb-cli
target/release/nxb build /path/Game.nsp --platform ios --keys /path/prod.keys --host-app /path/Nxkit.app --out /path/Game-unsigned.ipa
```

Choose a new output path. The producer adds the game manifest and assets at the
app root and the guest library at `Frameworks/nxgame.dylib`. Saves and caches
live outside the signed app. An unsigned IPA is not directly installable; the
complete app and guest library need valid device signing and provisioning.
Do not confuse a successful library build with shell linking, installation,
guest boot or gameplay validation.

## Checks

```sh
cargo test --locked -p nxkit-host -p nxkit-svc-input --lib
python3 -m unittest discover -s tools/ios/tests -v
```

Swift touch tests use `shell/TouchState.swift` and `shell/tests/main.swift`.
The shell's Apple SDK compile/link and device lifecycle/input tests are separate
checks. No source-only check produces an app or upgrades an embedded binary.

## Conversion limits

The producer accepts one NSP or XCI containing one base title and at most one
bundled update. Extracted data is capped at 256 MiB. Separate update inputs,
unsupported shader formats and title-specific services remain limitations.
Structural fixtures do not prove guest execution or gameplay.
