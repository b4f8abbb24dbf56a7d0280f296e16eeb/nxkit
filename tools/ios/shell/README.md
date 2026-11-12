# Swift app shell

`App.swift` owns UIApplication, the single UIWindowScene, startup errors and
foreground/background/disconnect handling. `MetalView.swift` owns the real
CAMetalLayer drawable size and UIKit touch routing. `TouchState.swift` has no
UIKit dependency and is tested locally on the build Mac before the app links.

The Rust static library exposes only the C functions in
`crates/nxkit-host/include/nxkit_ios.h`. Calls stay on the main thread; strings, buffers and
touch arrays are borrowed only during each call. Rust retains the Metal layer
and owns the runtime session. No Rust object or allocator ownership crosses C.
The shell never loads a guest dylib or implements kernel/GPU services itself.

Only one game may initialize per process. Disconnect pauses input/audio/rendering
and requests process shutdown; it is not a library navigation/reset flow. The
session and guest mappings are kept alive until process exit because the shared
kernel has process-lifetime state and detached guest threads.

Touches have stable IDs in 0..<16 and normalized picture coordinates. The current
presenter fills the drawable, so touches map to the actual full view without an
assumed 16:9 rectangle. Aspect-fit mapping is available only if the native
presenter reports that it actually uses it. New touches in bars are rejected;
existing drags clamp to the edge. Rotation, inactivity and cancellation release
contacts. These portable tests do not establish physical device input delivery.

## Single-game overlay

The overlay and layout picker adapt nxnative's `DmGamepad.m`, `DmLayout.h`
and `GameMenu.swift`.
The source mapping and full MIT notice are in the root `THIRD_PARTY.md`, which
is bundled into generated apps alongside the root `LICENSE`.

The gear offers 2D, 3D and (in handheld mode) Tap, touch feedback, and confirmed
Quit. Layout and feedback choices persist in this single-game app's defaults.
NX2Apple-specific size, stick-placement, toggle-key and geometry extensions
are not included. The layout editor, phone-motion implementation, controller
applets, TV/split-screen rendering and launcher-only settings are not ported.
Physical controllers, rumble and motion continue through nxkit, with no second
set of GameController handlers. A real physical controller hides the virtual
pad; a disconnect restores it after at most the 150 ms enumeration recheck.

`nx2ipa_virtual_pad` sends a complete P1 snapshot through a small explicit Rust
adapter. The existing nxkit Npad builder still produces guest input. Buttons
combine with physical P1, and each stick axis uses the stronger magnitude.
Unknown bits and nonfinite axes are sanitized. Focus loss, rotation, safe-area
changes, menu opening, layout changes, controller changes, cancellation and Quit
release controls and guest touches. UIKit does not own guest memory or threads.
Quit uses the existing process-stop watchdog, which can take up to five seconds;
a closing message covers that interval. It does not return to a game library.

Portable launch-log regressions run with
`python3 -m unittest discover -s tools/ios/tests -v`.
These do not exercise UIKit, multitouch delivery or menu animation. The Rust
input/host tests and an iOS-target Rust check are separate gates. The final
Swift/Objective-C SDK compile/link, gesture/lifecycle interaction tests and
physical-device gameplay remain required. No source-only check produces a new
host IPA or upgrades an already embedded binary.

## Launch diagnostics

Before native initialization, the shell redirects stderr directly to a private
`Library/Application Support/LaunchDiagnostics/launch.log` file. Rust fatal
messages are written synchronously to that descriptor, including an immediate
`exit(1)`; there is no asynchronous pipe reader to lose the final message.
Native startup emits stage markers without dumping game files or environment
variables. Existing runtime stderr may contain local paths, addresses, and game
diagnostic messages. Review a log before sharing it. Nothing is uploaded
automatically. The directory is excluded from backups.

Capture stops after ten seconds on a background deadline, or immediately after
native startup returns an error, and restores the original stderr descriptor.
It does not record stderr for an entire gameplay session. Finalized logs retain
only their last 256 KiB. Each launch rotates the last log to `previous-launch.log`
(two logs retained). An early process exit can leave a larger file until the next
launch trims it before recovery/sharing. The active capture has a time bound,
not a hard byte bound; disk errors and process termination can interrupt cleanup.
If the preceding native startup did not reach its ten-second checkpoint, the next launch stays on a recovery screen with **Share launch log**
and **Try launching again** instead of immediately repeating the failure.
Closing the app early can also trigger this screen; it is not proof of a crash.
A returned native-startup error also provides a **Share launch log** button.
The checkpoint only measures process survival, not gameplay correctness. Fatal
errors after capture ends are not captured. Failures after the checkpoint do
not trigger the early-startup screen. This is startup diagnostics, not a
continuous gameplay crash recorder.
Log creation failure is reported through NSLog and does not block the game.

Portable capture tests: `python -m unittest discover -s tools/ios/tests -v`.
These tests exercise the actual descriptor-redirection C helper, including its
final line on exit status 1, stderr restoration, and retained-tail byte limits.
UIKit recovery/share UI still requires an Apple SDK compile and device or iOS-on-Mac runtime verification.
