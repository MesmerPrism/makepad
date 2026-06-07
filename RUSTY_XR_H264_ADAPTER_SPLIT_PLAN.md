# Rusty XR External H264 Adapter Split Plan

This is a mechanical split preflight for
`tools/cargo_makepad/src/android/java/dev/makepad/android/BrokerH264VideoPlayer.java`.
It records responsibilities, target helper classes, invariants, and validation
before moving Java code.

The goal is pressure release, not behavior change. `BrokerH264VideoPlayer`
should remain the package-private compatibility facade until downstream
generated activity code and public Rusty XR examples no longer reference that
name.

## Current Facade Contract

Keep these stable during the first split intervals:

- package: `dev.makepad.android`;
- facade class: `BrokerH264VideoPlayer extends VideoPlayer`;
- constructor: `BrokerH264VideoPlayer(Activity activity, long videoId, Config config)`;
- entrypoint from `MakepadActivity.prepareBrokerH264VideoPlayback`;
- native callbacks through `MakepadNative`;
- Manifold defaults:
  - `rusty.manifold.command.envelope.v1`;
  - `/manifold/v1/events`;
  - `RMANVID1`;
- explicit legacy aliases:
  - `rusty.xr.broker.command.v1`;
  - `RXYRVID1`;
- `max_packets=0` remains live/unbounded.

Do not add a Manifold runtime dependency to Makepad. This adapter is still a
generic Java socket/media adapter inside the Makepad Android shell.

## Current Responsibility Map

| Responsibility | Current location | Split direction |
| --- | --- | --- |
| VideoPlayer facade and lifecycle | class fields, constructor, `prepareVideoPlayback`, `beginPlayback`, `pausePlayback`, `resumePlayback`, `stopAndCleanup`, `runDecode` | Keep in `BrokerH264VideoPlayer.java` as orchestration. |
| Output-mode decisions | `hasExternalTextureHandle`, `usesSurfaceTextureOutput`, `usesHardwareBufferOutput`, `usesCpuYuvOutput`, `effectiveDecodeOutputMode` | Keep in facade for first split, then move to config only if it reduces duplication. |
| Manifold command WebSocket | `sendStartCommand`, HTTP upgrade text, ack loop, `sendMaskedTextFrame`, `readWebSocketTextFrame`, `readHttpLine`, length helpers | `ManifoldH264CommandClient.java`. |
| Manifold command JSON | `startCommandJson` plus schema/path constants | Static builder inside `ManifoldH264CommandClient.java`. |
| Stream TCP connection | `connectWithRetry`, `mStreamSocket` ownership | Keep socket field in facade; helper may own connection attempt after command client is split. |
| Stream header/framing | `readHeader`, `readPacket`, `StreamHeader`, `Packet` | `ManifoldVideoStreamReader.java`, preserving `RMANVID1` default and `RXYRVID1` legacy read. |
| H.264 primer parsing | `findNalUnit`, `findStartCode`, `startCodeLengthAt`, `NalUnit` | `H264AnnexBPrimer.java`. |
| MediaCodec decode loop | `decodeStream`, `queuePacket`, `requestDecoderLowLatency`, `maybeLogProgress`, `logProgress` | Leave until command/stream/config helpers are split; then consider `ExternalH264DecoderLoop.java`. |
| CPU-YUV output | `emitYuvFrame`, `copyPlane` | `ExternalH264CpuYuvEmitter.java` after decoder-loop boundaries are stable. |
| Hardware-buffer output | `DecodeHardwareBufferTarget`, `HardwareBufferFrame` | `ExternalH264HardwareBufferTarget.java`. |
| Stereo HWB pairing | `StereoHardwareBufferPairer`, static pairer map and lock | `ExternalH264StereoPairer.java`; keep static map semantics. |
| Config and normalization | `Config`, normalize methods, clamp, defaults | `ExternalH264Config.java`; first code split candidate. |
| Completion/prepared callbacks | `notifyPrepared`, `notifyCompleted`, error callback paths | Keep in facade or a small callback helper only after decoder split. |

## First Code Slice

Status: completed. `ExternalH264Config.java` now owns config defaults,
normalization, clamp behavior, decode-output constants, source-sampling
constants, and `MAX_STREAM_PACKETS`. `BrokerH264VideoPlayer.Config` remains as
a thin nested compatibility facade for existing `MakepadActivity` wiring.

Recommended first movement:

1. Add `ExternalH264Config.java`.
2. Move `Config`, clamp, source/output/projection/sampling normalizers, and
   decode-output constants needed by config.
3. Keep a compatibility nested alias only if needed for call-site churn; prefer
   updating `MakepadActivity` to pass `ExternalH264Config` only when that
   remains package-private and mechanical.
4. Verify defaults are byte-for-byte equivalent in code review:
   - broker host/port/stream port;
   - source mode;
   - decode output mode;
   - projection profiles;
   - max packet semantics;
   - stereo pair role/max delta;
   - timeouts.

Do not move MediaCodec loop or HWB pairing in the same commit.

## Second Code Slice

Status: completed. `ManifoldH264CommandClient.java` now owns the command
WebSocket upgrade, command ack loop, command-envelope JSON builder, Manifold
schema/path defaults, explicit legacy command-schema alias, and command-only
WebSocket frame helpers.

Completed movement:

1. Add `ManifoldH264CommandClient.java`.
2. Move command schema/path constants, `sendStartCommand`, command JSON
   construction, WebSocket text frame helpers, and HTTP line helpers.
3. Keep request fields and JSON key spellings unchanged.
4. Keep the generated request id, client id, app label, and command names
   unchanged.
5. Keep close-on-ack behavior unchanged.

This slice leaves packet reading and decode loop inside the facade.

## Later Slices

After the config and command-client slices are validated and pushed:

1. Split stream framing into `ManifoldVideoStreamReader.java`.
2. Split H.264 Annex-B primer helpers.
3. Split hardware-buffer target and stereo pairer.
4. Split CPU-YUV emitter only if the decoder loop remains too broad.
5. Split decoder loop last, if needed.

## Validation

For documentation-only preflight changes:

```powershell
python tools\rusty_xr_format.py --changed --check
cargo metadata --no-deps --format-version 1
git diff --check
```

For Java movement:

```powershell
python tools\rusty_xr_format.py --changed --check
cargo metadata --no-deps --format-version 1
cargo check -p cargo-makepad
```

Also compile touched Java classes against the selected Android platform jar
before relying on downstream APK behavior. If generated Android template or
packager linkage changes, reinstall `cargo-makepad` from this checkout before
building the downstream public Rusty XR Makepad APK.

For behavior changes, add the downstream source-root APK build and Quest/device
validation only after the host-side Java/Cargo checks pass.

## Stop Conditions

Stop and reassess before committing if a split requires any of these:

- changing stream schema, packet fields, or JSON command names;
- removing legacy aliases;
- changing native callback order or callback payloads;
- changing MediaCodec surface/CPU-YUV/HWB output behavior;
- adding new Makepad dependencies;
- moving command/session/stream authority into Makepad.
