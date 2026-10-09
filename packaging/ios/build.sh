#!/usr/bin/env bash
# Build PhotoCraft for iPad, bundle it as PhotoCraft.app, and optionally install and run it on
# the iPad Simulator or a real iPad. macOS with Xcode only. See docs/ios.md.
#
# Usage: packaging/ios/build.sh [--simulator | --device] [--release] [--run] [--features heif]
#
#   --simulator   (default) build for the iPad Simulator (aarch64-apple-ios-sim, Apple silicon
#                 Macs), ad-hoc signed. With --run: boot an iPad simulator, install, launch.
#   --device      build for a real iPad (aarch64-apple-ios), signed with your Apple Development
#                 identity and provisioning profile. With --run: install over USB/Wi-Fi with
#                 `xcrun devicectl` and launch with the app's log in this terminal.
#   --release     optimized build (default: debug, which is slow for painting; use it to debug).
#   --run         install and launch after building.
#   --features F  cargo features (e.g. heif), as for the desktop app.
#
# Output: $DIST/ios/<simulator|device>/PhotoCraft.app
#
# Environment:
#   IOS_BUNDLE_ID        bundle identifier (default ai.storyteller.photocraft). A free Apple ID
#                        ("Personal Team") needs one of your own, e.g. com.yourname.photocraft.
#   IOS_SIGN_IDENTITY    --device: codesign identity, e.g. "Apple Development: you@example.com
#                        (ABCDE12345)" (`security find-identity -v -p codesigning` lists them).
#                        Default: the first "Apple Development" identity.
#   IOS_PROFILE          --device: the .mobileprovision for IOS_BUNDLE_ID. Default: the newest
#                        installed profile (Xcode's folders) whose app id matches.
#   IOS_DEVICE           --device --run: the iPad's name or UDID (`xcrun devicectl list devices`).
#   IOS_SIMULATOR        --simulator --run: the simulator's UDID (default: the first available
#                        iPad in `xcrun simctl list devices available`).
#   IPHONEOS_DEPLOYMENT_TARGET  minimum iPadOS (default 17.0; keep in sync with docs/ios.md).
set -euo pipefail
# shellcheck source=../env.sh
. "$(dirname "${BASH_SOURCE[0]}")/../env.sh"
HERE="$ROOT/packaging/ios"

KIND=simulator
PROFILE_NAME=debug
RUN=0
FEATURES=()
while [ $# -gt 0 ]; do
  case "$1" in
    --simulator) KIND=simulator; shift ;;
    --device) KIND=device; shift ;;
    --release) PROFILE_NAME=release; shift ;;
    --run) RUN=1; shift ;;
    --features) FEATURES=(--features "$2"); shift 2 ;;
    -h | --help) sed -n '2,32p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

if [ "$(uname -s)" != Darwin ] || ! command -v xcrun >/dev/null; then
  echo "error: building for iPad needs macOS with Xcode (xcrun not found)" >&2
  exit 1
fi

case "$KIND" in
  simulator) TARGET=aarch64-apple-ios-sim; PLATFORM=iPhoneSimulator; SDK=iphonesimulator ;;
  device) TARGET=aarch64-apple-ios; PLATFORM=iPhoneOS; SDK=iphoneos ;;
esac
BUNDLE_ID="${IOS_BUNDLE_ID:-ai.storyteller.photocraft}"
export IPHONEOS_DEPLOYMENT_TARGET="${IPHONEOS_DEPLOYMENT_TARGET:-17.0}"
SHORT_VERSION="${VERSION%%-*}"

# 1. Build.
if command -v rustup >/dev/null; then
  rustup target add "$TARGET" >/dev/null
fi
# rustc and cc-rs find the iOS SDK with xcrun themselves; check it's installed.
xcrun --sdk "$SDK" --show-sdk-path >/dev/null || { echo "error: the $SDK SDK is missing (install Xcode's iOS platform)" >&2; exit 1; }
CARGO_ARGS=(build -p photocraft-ios --target "$TARGET" ${FEATURES[@]+"${FEATURES[@]}"})
if [ "$PROFILE_NAME" = release ]; then CARGO_ARGS+=(--release); fi
echo "Building photocraft-ios for $TARGET ($PROFILE_NAME, iPadOS $IPHONEOS_DEPLOYMENT_TARGET+)"
(cd "$ROOT" && cargo "${CARGO_ARGS[@]}")
BIN="$CARGO_TARGET_DIR/$TARGET/$PROFILE_NAME/photocraft-ios"
[ -x "$BIN" ] || { echo "error: $BIN was not built" >&2; exit 1; }

# 2. Bundle.
OUT="$DIST/ios/$KIND"
APP="$OUT/PhotoCraft.app"
rm -rf "$APP"
mkdir -p "$APP"
cp "$BIN" "$APP/PhotoCraft"
sed -e "s|@VERSION@|$VERSION|g" \
  -e "s|@SHORT_VERSION@|$SHORT_VERSION|g" \
  -e "s|@BUILD_SHA@|${PHOTOCRAFT_BUILD_SHA:-}|g" \
  -e "s|@BUNDLE_ID@|$BUNDLE_ID|g" \
  -e "s|@PLATFORM@|$PLATFORM|g" \
  -e "s|@MIN_OS@|$IPHONEOS_DEPLOYMENT_TARGET|g" \
  "$HERE/Info.plist.in" >"$APP/Info.plist"
plutil -lint "$APP/Info.plist" >/dev/null
printf 'APPL????' >"$APP/PkgInfo"
# App icons (iPad: 76 pt @2x, 83.5 pt @2x) from the committed 1024 px master.
ICON="$ROOT/assets/app-icon/photocraft-1024.png"
if [ -f "$ICON" ]; then
  sips -z 152 152 "$ICON" --out "$APP/AppIcon76x76@2x~ipad.png" >/dev/null
  sips -z 167 167 "$ICON" --out "$APP/AppIcon83.5x83.5@2x~ipad.png" >/dev/null
fi
copy_docs "$APP"

# 3. Sign.
if [ "$KIND" = simulator ]; then
  codesign --force --sign - --timestamp=none "$APP"
else
  IDENTITY="${IOS_SIGN_IDENTITY:-$(security find-identity -v -p codesigning | awk -F'"' '/Apple Development/ { print $2; exit }')}"
  if [ -z "$IDENTITY" ]; then
    echo "error: no Apple Development signing identity; add your Apple ID in Xcode › Settings › Accounts (see docs/ios.md)" >&2
    exit 1
  fi
  TMP="$(mktemp -d)"
  trap 'rm -rf "$TMP"' EXIT
  # The newest installed profile whose application-identifier is <team>.<bundle id> (or a
  # wildcard), from Xcode 16's folder or the older one.
  find_profile() {
    local p app newest="" newest_time=0 t
    for p in "$HOME/Library/Developer/Xcode/UserData/Provisioning Profiles"/*.mobileprovision \
      "$HOME/Library/MobileDevice/Provisioning Profiles"/*.mobileprovision; do
      [ -f "$p" ] || continue
      security cms -D -i "$p" >"$TMP/p.plist" 2>/dev/null || continue
      app="$(/usr/libexec/PlistBuddy -c 'Print :Entitlements:application-identifier' "$TMP/p.plist" 2>/dev/null || true)"
      case "$app" in
        *."$BUNDLE_ID" | *.\*) ;;
        *) continue ;;
      esac
      t="$(stat -f %m "$p")"
      if [ "$t" -gt "$newest_time" ]; then newest="$p"; newest_time="$t"; fi
    done
    printf '%s' "$newest"
  }
  PROFILE="${IOS_PROFILE:-$(find_profile)}"
  if [ -z "$PROFILE" ] || [ ! -f "$PROFILE" ]; then
    echo "error: no provisioning profile for $BUNDLE_ID; set IOS_PROFILE, or let Xcode make one (see docs/ios.md)" >&2
    exit 1
  fi
  echo "Signing with \"$IDENTITY\" and $(basename "$PROFILE")"
  cp "$PROFILE" "$APP/embedded.mobileprovision"
  security cms -D -i "$PROFILE" >"$TMP/profile.plist"
  /usr/libexec/PlistBuddy -x -c 'Print :Entitlements' "$TMP/profile.plist" >"$TMP/entitlements.plist"
  codesign --force --sign "$IDENTITY" --entitlements "$TMP/entitlements.plist" --timestamp=none "$APP"
fi
codesign --verify --strict "$APP"
echo "Built $APP"

[ "$RUN" = 1 ] || exit 0

# 4. Install and run.
if [ "$KIND" = simulator ]; then
  SIM="${IOS_SIMULATOR:-$(xcrun simctl list devices available | grep -m1 'iPad' | grep -oE '[0-9A-F]{8}-([0-9A-F]{4}-){3}[0-9A-F]{12}' || true)}"
  if [ -z "$SIM" ]; then
    echo "error: no iPad simulator; add one in Xcode › Window › Devices and Simulators" >&2
    exit 1
  fi
  xcrun simctl boot "$SIM" 2>/dev/null || true
  open -a Simulator --args -CurrentDeviceUDID "$SIM"
  xcrun simctl install "$SIM" "$APP"
  echo "Launching $BUNDLE_ID on simulator $SIM (Ctrl-C stops following the log)"
  xcrun simctl launch --console-pty --terminate-running-process "$SIM" "$BUNDLE_ID"
else
  if [ -z "${IOS_DEVICE:-}" ]; then
    xcrun devicectl list devices >&2 || true
    echo "error: set IOS_DEVICE to the iPad's name or UDID (listed above)" >&2
    exit 1
  fi
  xcrun devicectl device install app --device "$IOS_DEVICE" "$APP"
  echo "Launching $BUNDLE_ID on $IOS_DEVICE (Ctrl-C stops following the log)"
  xcrun devicectl device process launch --device "$IOS_DEVICE" --terminate-existing --console "$BUNDLE_ID"
fi
