# PhotoCraft for iPad

`apps/photocraft-ios` runs the same `PhotocraftApp` as the desktop app on the iPad, natively: eframe's
winit runner on UIKit, wgpu on Metal. It is Rust only, like the desktop and web builds (no Swift, no
Xcode project, no webview).

**Status: first cut.** It builds for the device and the simulator, but it has not been run on
hardware by the project yet. Expect rough edges, and please report what you find (see
[Testing on a real iPad](#testing-on-a-real-ipad)).

## What it does

| Area | How |
|---|---|
| Apple Pencil pressure | `force / maximumPossibleForce` of each Pencil `UITouch` (`crates/tablet/src/uikit.rs`); winit's touch force is the fallback |
| Tilt | altitude and azimuth converted to Pointer Events `tiltX` / `tiltY` (drives brush controls set to Pen Tilt) |
| Barrel roll | `rollAngle` (Apple Pencil Pro, iPadOS 17.5) as rotation (brush controls set to Rotation) |
| Hover | `UIHoverGestureRecognizer`: the brush cursor follows the Pencil above the glass (M2 iPad Pro / iPad Air and later) |
| Double-tap, squeeze | `UIPencilInteraction`, doing what Settings › Apple Pencil asks: eraser toggle, last tool, Color Picker, Brush Settings (`apps/photocraft-ios/src/pencil.rs`) |
| Palm rejection | fingers are ignored while the Pencil is down |
| Fingers | one finger on panels taps and drags; two fingers pan and pinch-zoom; a two-finger tap is Undo, three fingers Redo (`apps/photocraft-ios/src/touch.rs`) |
| One finger on the canvas | paints like a mouse until the Pencil is first used (or with Settings › Apple Pencil › Only Draw with Apple Pencil on), then pans |
| Files | File › Open and Save As browse the app's Documents folder, which the Files app shows as *On My iPad › PhotoCraft* |
| Crash recovery | autosaves and recovery as on the desktop, under `Library/Application Support/PhotoCraft` in the app container |

The Pencil glue (`crates/tablet/src/ios.rs`) is the second module of the isolated `photocraft-tablet`
crate that allows `unsafe`, for the same reason as the macOS one: subclassing UIKit classes needs
Objective-C interop. It adds a touch-observing gesture recognizer (it sees each touch before winit's
view and never cancels or delays it), a hover recognizer and a `UIPencilInteraction` to winit's view,
and writes into the same `StylusFeed` the desktop tablet readers use. All mapping is pure and tested
on every platform (`cargo test -p photocraft-tablet`, `cargo test -p photocraft-ios`).

## Requirements

- A Mac with Xcode 16 or later (`xcrun devicectl … --console` needs it), selected with
  `sudo xcode-select -s /Applications/Xcode.app`.
- Rust with the iOS targets: `rustup target add aarch64-apple-ios aarch64-apple-ios-sim`
  (`packaging/ios/build.sh` adds them). The simulator target is for Apple-silicon Macs.
- An iPad on iPadOS 17.0 or later. Any Apple Pencil works for pressure and tilt; double-tap needs
  Apple Pencil (2nd generation) or Pro; squeeze and barrel roll need Apple Pencil Pro; hover needs a
  hover-capable iPad.

## 1. Try the shell on the Mac

The fastest loop needs no simulator: the iPad shell also runs as a desktop window (iPad Pro 11"
landscape size), with its settings and documents in a temporary folder.

```sh
cargo run -p photocraft-ios
```

The mouse acts as a mouse here; a touch screen (on Linux or Windows) exercises the touch rules.

## 2. The iPad Simulator

```sh
packaging/ios/build.sh --simulator --run
```

This builds for `aarch64-apple-ios-sim`, bundles `dist/release/ios/simulator/PhotoCraft.app`, boots
the first available iPad simulator (or `IOS_SIMULATOR=<udid>`), installs, launches, and streams the
app's log. In the simulator the mouse is one finger and ⌥-drag is a two-finger pinch. There is no
pressure, so use a real iPad for the Pencil.

## 3. A real iPad

### One-time setup

1. **Apple ID in Xcode**: Xcode › Settings › Accounts › +. A free Apple ID gives a "Personal Team":
   apps you install expire after 7 days (just rebuild). A paid Apple Developer Program membership
   lasts a year.
2. **Connect the iPad** with a cable, unlock it and tap *Trust*. Open Xcode › Window › Devices and
   Simulators and wait until the iPad is ready. Optionally tick *Connect via network* for wireless
   installs later.
3. **Developer Mode** on the iPad: Settings › Privacy & Security › Developer Mode › On, restart,
   confirm. (The switch appears once Xcode has seen the iPad.)
4. **Pick a bundle identifier of your own**, e.g. `com.yourname.photocraft` (a Personal Team can't
   use `ai.storyteller.photocraft`).
5. **Let Xcode make a signing certificate and provisioning profile for it**, once: Xcode › File ›
   New › Project › iOS › App; any name; in the target's *Signing & Capabilities* choose your team
   and set *Bundle Identifier* to the one from step 4; pick your iPad as the run destination and
   press Run. That installs an empty app (delete it afterwards) and leaves an "Apple Development"
   certificate in your keychain and an "iOS Team Provisioning Profile" for the bundle id, which
   `build.sh` finds by itself.

### Build, install, run

```sh
xcrun devicectl list devices                    # your iPad's name or UDID
export IOS_BUNDLE_ID=com.yourname.photocraft
export IOS_DEVICE="Your iPad"
packaging/ios/build.sh --device --release --run
```

`--release` matters: debug builds are too slow to paint with. The script builds for
`aarch64-apple-ios`, bundles and signs `dist/release/ios/device/PhotoCraft.app` (identity and
profile can be forced with `IOS_SIGN_IDENTITY` and `IOS_PROFILE`), installs it with `xcrun devicectl`
and launches it with its log in the terminal (Ctrl-C detaches). Run `packaging/ios/build.sh --help`
for every option.

With a free account the first launch is refused as an "Untrusted Developer": on the iPad open
Settings › General › VPN & Device Management, tap your Apple ID and *Trust*, then run again.

## Testing on a real iPad

Open or create a document and work through this list. When something is off, note the iPad model,
Pencil model, iPadOS version, and the terminal log.

| Check | How | Expected |
|---|---|---|
| Pressure | Brush tool; Brush Settings › Shape Dynamics › Size Jitter › Control: Pen Pressure, and Transfer › Opacity Jitter › Control: Pen Pressure; draw light to hard | thin and faint to wide and opaque, smoothly |
| Tilt | Control: Pen Tilt on size or angle; draw upright, then tilted | the stroke changes with tilt, the same way when tilting left and right as when tilting towards and away from you |
| Barrel roll (Pencil Pro) | Control: Rotation on the brush angle; roll the Pencil while drawing | the tip turns with the barrel |
| Hover | hold the Pencil just above the glass | the brush cursor follows it |
| Double-tap | Settings › Apple Pencil › Double-tap: *Switch between current tool and eraser*; double-tap | Eraser, then back to the Brush; try *last used* and *color palette* (Color Picker) too |
| Squeeze (Pencil Pro) | Settings › Apple Pencil › Squeeze | as for double-tap, plus Brush Settings for the palette actions |
| Palm rejection | rest your hand on the glass while drawing | no zoom, pan or stray marks |
| Fingers | two-finger drag, pinch, two-finger tap, three-finger tap | pan, zoom around the fingers, Undo, Redo |
| Finger painting | before using the Pencil, drag one finger on the canvas | paints; after the Pencil was used, one finger pans |
| Panels and menus | tap menus and buttons, drag sliders, scroll panels with two fingers | work as with a mouse |
| Files | Files app › On My iPad › PhotoCraft: copy a PSD in; File › Open; File › Save As | the PSD opens; saved files appear in the Files app |
| Recovery | paint, swipe the app away from the app switcher, start it again | the document is offered for recovery |
| Rotation | turn the iPad | the window follows; nothing is cut off under the home indicator |

## Known gaps

- **Hardware keyboards**: winit 0.30 on iOS reports typed text but no modifier keys or key presses,
  so ⌘Z and other shortcuts don't work yet. Use the gestures, menus and the Pencil.
- **No touch modifier**: Photoshop on the iPad has an on-screen "touch shortcut" for ⇧ / ⌥; PhotoCraft
  doesn't yet, so ⌥-click eyedropping and similar need a menu or tool instead.
- **Files**: only the app's Documents folder; no system document picker, no Photos library, no "Open in
  PhotoCraft" from other apps yet.
- **Clipboard**: no exchange with other apps (egui-winit has no iOS clipboard).
- **Interface density**: the desktop layout; raise the Interface preferences' UI scale for bigger
  touch targets.
- **Split View and Stage Manager**: off (`UIRequiresFullScreen`).
- **Automation**: the control server and MCP don't run on the iPad.
- **Distribution**: development installs only; TestFlight and the App Store need an asset-catalog
  icon, an archive step and signing secrets in CI.

## Continuous integration

The `iPad build` job in `.github/workflows/ci.yml` runs on a macOS runner: clippy for
`photocraft-ios` and `photocraft-tablet` on `aarch64-apple-ios`, then `packaging/ios/build.sh
--simulator`, which builds, bundles and signs the simulator app. Nothing runs there (no iPad).
