# Rusty XR Makepad Activity Split Plan

This is a mechanical split preflight for
`tools/cargo_makepad/src/android/java/dev/makepad/android/MakepadActivity.java`.

The goal is pressure release, not Android lifecycle behavior change.
`MakepadActivity` should remain the generated Android activity facade while
Rusty-owned app-shell helper families move into package-private helpers under
`dev.makepad.android`.

## Current Facade Contract

Keep these stable during split intervals:

- package: `dev.makepad.android`;
- public activity class: `MakepadActivity extends Activity`;
- generated template hooks such as `//% MAIN_ACTIVITY_BODY`,
  `//% MAIN_ACTIVITY_ON_ACTIVITY_RESULT`, and lifecycle plugin hooks;
- static library load order around `System.loadLibrary("makepad")`;
- native callbacks through `MakepadNative`;
- activity switching behavior and intent-extra forwarding;
- video playback map/thread ownership in the activity;
- `prepareBrokerH264VideoPlayback` public entrypoint and argument order;
- MediaProjection request code, default delay, default host/port/size, and
  foreground-service payload extras;
- Rusty activity phase-marker schema text and phase names.

Do not add Manifold, Hostess, Studio, Quest runtime authority, sockets, or
app-specific policy to `MakepadActivity`. Manifold remains command/session
authority; this file is Android app-shell glue.

## Responsibility Map

| Responsibility | Current location | Split direction |
| --- | --- | --- |
| Upstream Android lifecycle and view setup | `onCreate`, `onResume`, `onPause`, `onDestroy`, surface/layout methods | Keep in `MakepadActivity.java`; do not broad-refactor. |
| Rusty activity phase markers | `rustyXrActivityMarker`, static load markers, onCreate/native onCreate markers | Move marker formatting/logging into `RustyXrActivitySupport.java`, keep phase call sites in the activity. |
| Rusty intent-extra parsing | `rustyXrIntentBooleanExtra`, `rustyXrIntentIntExtra`, `rustyXrIntentLongExtra` | Move typed parsing into `RustyXrActivitySupport.java`, passing an explicit `Intent`. |
| MediaProjection request/result flow | `mRustyXrMediaProjectionManager`, request constants, `requestRustyXrMediaProjectionIfEnabled`, `requestRustyXrMediaProjection`, MediaProjection branch in `onActivityResult`, service stop in `onDestroy` | Move to `RustyXrMediaProjectionHelper.java`; activity keeps lifecycle call sites and delegates request/result handling. |
| H264/external-video entrypoint | `prepareBrokerH264VideoPlayback` config construction and runnable insertion | Later helper only if it can preserve the public method signature and video-thread/map ownership. |
| Generic video playback | `prepareVideoPlayback`, `beginVideoPlayback`, pause/resume/stop, cleanup | Keep in activity unless a video-shell boundary becomes necessary. |
| Activity switching | `switchActivityClass`, intent extras forwarding | Keep in activity because it is upstream lifecycle/app-shell behavior. |

## Current Slice Status

- Preflight/source map: complete.
- `RustyXrActivitySupport.java`: complete. Owns Rusty activity marker
  formatting/logging and typed Rusty intent-extra parsing.
- `RustyXrMediaProjectionHelper.java`: complete. Owns MediaProjection enable
  parsing, delayed consent request, consent result handling, foreground-service
  payload construction, and service stop glue.
- `MakepadActivity.java`: remains the generated activity facade and keeps
  lifecycle order, plugin hooks, native callbacks, public H264 entrypoint, video
  map/thread ownership, and activity switching.
- Next candidate: only preflight `prepareBrokerH264VideoPlayback` if H264
  entrypoint pressure grows; do not continue splitting lifecycle code by line
  count.

## First Slice

Recommended first movement:

1. Add `RustyXrActivitySupport.java`.
2. Move activity phase-marker formatting/logging and typed Rusty intent-extra
   parsing into static package-private helper methods.
3. Keep all phase call sites and MediaProjection call sites in
   `MakepadActivity.java`.
4. Preserve marker schema, phase names, log tag, boolean string handling, number
   parsing, fallback behavior, and swallowed parse exceptions.

## Second Slice

Recommended second movement after the first slice validates:

1. Add `RustyXrMediaProjectionHelper.java`.
2. Move MediaProjection request code/default delay, manager lookup,
   enabled/delay parsing, consent request, consent result handling, foreground
   service intent construction, and service stop helper.
3. Keep `MakepadActivity` lifecycle order unchanged:
   - construct helper after `super.onCreate`;
   - request if enabled at the existing onCreate/onNewIntent call sites;
   - delegate only the MediaProjection request branch in `onActivityResult`;
   - stop the stream service from `onDestroy`.
4. Preserve request code, default delay, default host/port/width/height,
   foreground-service extras, denial/grant log strings, and fallback behavior.

## Later Slices

Later movement needs a separate preflight before code changes:

1. H264/external-video entrypoint helper. Only extract config construction if
   `prepareBrokerH264VideoPlayback` keeps its public signature and the activity
   keeps video runnable map/thread ownership.
2. Additional diagnostic marker cleanup. Prefer compatibility aliases and
   explicit historical marker names over broad Rusty-XR renames.

Do not split Android lifecycle, surface recovery, keyboard/input, selection,
camera preview overlay, network thread ownership, or activity switching by line
count alone.

## Validation

For documentation-only preflight changes:

```powershell
python tools\rusty_xr_format.py --changed --check
cargo metadata --no-deps --format-version 1
git diff --check
```

For Java helper movement:

```powershell
python tools\rusty_xr_format.py --changed --check
cargo metadata --no-deps --format-version 1
cargo check -p cargo-makepad
javac -cp S:\Work\tools\Android\windows-sdk\platforms\android-35\android.jar -d <temp> `
  target\android\makepad-android-apk\makepad_example_uizoo\tmp\dev\makepad\android\R.java `
  tools\cargo_makepad\src\android\java\dev\makepad\android\*.java
git diff --check
git diff --cached --check
```

For grouped push validation after source movement, add:

```powershell
cargo test -p cargo-makepad
cargo build -p cargo-makepad --release
```

Do not treat Quest/device validation as required for mechanical helper
movement unless generated activity behavior or runtime MediaProjection behavior
changes.

## Stop Conditions

Stop and reassess before committing if a split requires any of these:

- changing lifecycle order or plugin hook positions;
- changing public `MakepadActivity` method signatures;
- changing intent-extra keys, defaults, or forwarding behavior;
- changing MediaProjection request code, service extras, or consent flow;
- changing native callback order;
- adding Manifold, Hostess, Studio, Quest runtime authority, sockets, or
  app-specific policy to this Android activity helper layer.
