# iOS shell

One app boots one bundled NVN game. Use a paired physical controller. There is no game picker, touch overlay, keyboard or mouse input.

On a Mac with the iPhoneOS 26 SDK and the Rust `aarch64-apple-ios` target:

```sh
tools/ios/build-host.sh
```

This builds only the shell executable. Put it in the iOS app as `nxkit` with
`Info.plist` and an independently prepared iOS game payload: `game.json`,
its referenced data, and a signed device dylib with the `__NX_HEAP` reservation.
The executable, payload, and signing are separate from `nxb build`.
A macOS `nxb` app is not an iOS payload.
