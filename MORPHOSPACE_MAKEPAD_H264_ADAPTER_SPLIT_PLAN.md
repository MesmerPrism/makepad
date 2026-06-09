# Morphospace Makepad External H264 Adapter Split Plan

This is a mechanical split preflight for
`tools/cargo_makepad/src/android/java/dev/makepad/android/BrokerH264VideoPlayer.java`.
It records responsibilities, target helper classes, invariants, and validation
before moving Java code.

The goal is pressure release, not behavior change. `BrokerH264VideoPlayer`
should remain the package-private compatibility facade until downstream
generated activity code and legacy/public Rusty XR examples no longer reference that
name.

## Current Facade Contract

Keep these stable during the first split intervals:

- package: `dev.makepad.android`;
- facade class: `BrokerH264VideoPlayer extends VideoPlayer`;
- constructor: `BrokerH264VideoPlayer(Activity activity, long videoId, Config config)`;
- entrypoint from `MakepadActivity.prepareBrokerH264VideoPlayback`, with
  config/player/runnable construction delegated through
  `ExternalH264VideoPlaybackFactory.java`;
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
| CPU-YUV output | `ExternalH264CpuYuvEmitter.java` | Completed. Facade keeps output-mode decision, image acquisition/close, copy timing, and progress counters. |
| Hardware-buffer output | `ExternalH264HardwareBufferTarget.java` | Completed. Facade keeps orchestration, surface ownership reference, and timeout selection. |
| Stereo HWB pairing | nested pairer family inside `ExternalH264HardwareBufferTarget.java` | Completed with the target because pairing owns retained `HardwareBuffer` frame lifetime and callback emission. |
| Config and normalization | `Config`, normalize methods, clamp, defaults | `ExternalH264Config.java`; first code split candidate. |
| Activity entrypoint construction | `ExternalH264VideoPlaybackFactory.java` | Completed. `MakepadActivity.java` keeps the public method signature, runnable map insertion, and handler post. |
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

## Third Code Slice

Status: completed. `ManifoldVideoStreamReader.java` now owns stream magic
constants, stream header validation, projection-metadata parsing/logging,
packet-size limits, packet read, and package-private stream header/packet DTOs.

Completed movement:

1. Add `ManifoldVideoStreamReader.java`.
2. Move `RMANVID1` default and explicit `RXYRVID1` legacy stream-header read.
3. Move stream header metadata parsing, packet read, `StreamHeader`, and
   `Packet`.
4. Keep TCP connection ownership, MediaCodec decode, H.264 primer parsing,
   CPU-YUV, HWB, and stereo pairing inside the facade.

## Fourth Code Slice

Status: completed. `H264AnnexBPrimer.java` now owns Annex-B start-code
scanning, SPS/PPS NAL lookup, and the package-private `NalUnit` DTO used to
seed `MediaFormat` CSD buffers.

Completed movement:

1. Add `H264AnnexBPrimer.java`.
2. Move `findNalUnit`, `findStartCode`, `startCodeLengthAt`, and `NalUnit`.
3. Keep primer-packet selection, stream reads, MediaCodec decode, CPU-YUV,
   HWB, and stereo pairing inside the facade.

## Fifth Code Slice

Status: completed. `ExternalH264HardwareBufferTarget.java` now owns the
ImageReader-backed hardware-buffer decode target, single-frame hardware-buffer
native callback emission, retained frame DTO, stereo hardware-buffer pairing
queues, pair drop logging, and the static pairer map/lock.

Completed movement:

1. Add `ExternalH264HardwareBufferTarget.java`.
2. Move `DecodeHardwareBufferTarget`, `HardwareBufferFrame`,
   `StereoHardwareBufferPairer`, pairer constants, pairer map, and lock.
3. Keep `BrokerH264VideoPlayer` as the decoder orchestrator: output-mode
   decision, MediaCodec release timing, hardware-buffer wait timeout, progress
   counters, and lifecycle cleanup call remain in the facade.
4. Preserve native callback order and payloads:
   `MakepadNative.onVideoHardwareBufferFrame` for single frames and
   `MakepadNative.onVideoHardwareBufferStereoFrame` for paired frames.

Follow-up: the pre-existing `clearStereoHardwareBufferPairerIfUnused` helper
was not called before this split. Do not fold that lifecycle cleanup into a
mechanical move; treat it as a behavior fix that needs downstream hardware-
buffer validation.

## Sixth Code Slice

Status: completed. `ExternalH264CpuYuvEmitter.java` now owns CPU-YUV plane
copying and `MakepadNative.onVideoYuvFrame` emission.

Completed movement:

1. Add `ExternalH264CpuYuvEmitter.java`.
2. Move `emitYuvFrame` and `copyPlane`.
3. Keep `BrokerH264VideoPlayer` as the decoder orchestrator: output-mode
   decision, `MediaCodec.getOutputImage`, `Image.close`, copy timing,
   progress counters, and decode-loop control flow remain in the facade.
4. Preserve `onVideoYuvFrame` argument order, width/height clamping, chroma
   dimensions, row/pixel-stride handling, and zero-fill behavior for missing
   plane bytes.

## Later Slices

After the config, command-client, stream-reader, Annex-B primer,
hardware-buffer target, and CPU-YUV emitter slices are validated and pushed:

1. Activity entrypoint construction is complete in
   `ExternalH264VideoPlaybackFactory.java`; keep activity map/thread ownership
   in `MakepadActivity.java`.
2. Split decoder loop last, if needed.
3. Consider stereo pairer lifecycle cleanup only as a behavior slice with
   downstream hardware-buffer validation.

## Decoder Loop Preflight

Do not split the decoder loop only because `BrokerH264VideoPlayer.java` is still
large. The remaining facade is currently cohesive enough: it owns player
lifecycle, stream socket ownership, output-mode decisions, MediaCodec
orchestration, image acquisition/close, progress counters, and prepared/error
callbacks.

Before moving decoder-loop code, write a fresh preflight that answers:

| Boundary question | Required answer before movement |
| --- | --- |
| Packet ownership | Which object owns `ManifoldVideoStreamReader.Packet` sequencing, primer packet selection, and packet exhaustion? |
| CSD and format setup | Does the new helper only consume `H264AnnexBPrimer.NalUnit` output, or does it also decide stream/header policy? |
| MediaCodec lifecycle | Which owner creates, starts, flushes, stops, releases, and handles low-latency configuration failures? |
| Output routing | Which owner decides surface-texture, hardware-buffer, and CPU-YUV output, and which owner closes `Image` objects? |
| Callback order | How are `notifyPrepared`, `notifyCompleted`, `onVideoYuvFrame`, `onVideoHardwareBufferFrame`, and stereo frame callbacks preserved? |
| Timing and errors | Which owner logs progress, decode errors, copy timing, source timestamps, and stale-stream failures? |

### Decoder Loop Ownership Map

Current answer: BrokerH264VideoPlayer.java remains the decoder orchestrator.
The helper boundary is not clean enough yet to move code safely without turning
a mechanical split into a behavior slice.

| Area | Current owner | Helper boundary only if split later |
| --- | --- | --- |
| Packet sequencing and exhaustion | `BrokerH264VideoPlayer.decodeStream` owns the read loop, packet exhaustion, max-packet/live semantics, and primer packet selection from `ManifoldVideoStreamReader.Packet`. | A helper may consume an already connected `ManifoldVideoStreamReader` and return a status object, but it must not change packet fields, `max_packets=0`, stream-header policy, or legacy `RXYRVID1` handling. |
| CSD/primer handoff | `BrokerH264VideoPlayer` owns primer packet choice and `MediaFormat` CSD attachment after calling `H264AnnexBPrimer`. | A helper may accept explicit `NalUnit` outputs and attach CSD buffers, but it must not decide stream schema, projection metadata, or primer search policy. |
| MediaCodec lifecycle | `BrokerH264VideoPlayer` creates, configures, starts, drains, stops, and releases `MediaCodec`, including low-latency requests and failure fallback. | A helper boundary is only clean if the facade still owns lifecycle policy and the helper only runs a bounded decode session with explicit release obligations. |
| Output routing and cleanup | `BrokerH264VideoPlayer` decides surface-texture, hardware-buffer, and CPU-YUV output modes; it owns `Image` acquisition/close and hardware-buffer wait timeout selection. | A helper may receive output adapters for CPU-YUV and HWB callbacks, but it must not move output-mode decisions or change `Image.close`/buffer release ordering. |
| Timing counters and stale-state evidence | `BrokerH264VideoPlayer` owns progress counters, decode error counts, copy timing, packet timestamps, stale-stream state, and progress log cadence. | A helper may return counters to the facade; it must not invent new success criteria or hide stale/failed decode state. |
| Prepared/completed/error callbacks | `BrokerH264VideoPlayer` owns `notifyPrepared`, `notifyCompleted`, error callbacks, and native callback order through `MakepadNative`. | A helper may report state transitions, but the facade must emit callbacks in the same order and with the same payloads. |
| Stop and cleanup | `BrokerH264VideoPlayer.stopAndCleanup` owns stop state, socket close, thread lifecycle, decoder release, surface release, and target cleanup. | A helper may expose an idempotent close hook only if cleanup order and repeated-stop behavior stay unchanged. |

Only move code if the answer is a narrow package-private helper such as
`ExternalH264DecoderLoop.java` that receives explicit dependencies and returns
status to the facade. If the split requires changing packet fields, stream
schema, callback order, MediaCodec output behavior, or legacy alias handling,
stop and treat it as a behavior slice.

## Activity Entrypoint Slice

Status: completed. `ExternalH264VideoPlaybackFactory.java` now owns external-
H264 config construction, `BrokerH264VideoPlayer` creation, playback flag
assignment, and runnable construction used by
`MakepadActivity.prepareBrokerH264VideoPlayback`.

Preserved boundary:

1. `MakepadActivity.prepareBrokerH264VideoPlayback` keeps its public signature
   and argument order.
2. `MakepadActivity.java` keeps `mVideoPlayerRunnables.put(...)` and
   `mVideoPlaybackHandler.post(...)` so activity video map/thread ownership
   does not move into the H264 adapter.
3. `BrokerH264VideoPlayer` remains the package-private compatibility facade
   until downstream generated activity code and public examples no longer
   reference that name.

## Validation

For documentation-only preflight changes:

```powershell
python tools\makepad_fork_format.py --changed --check
cargo metadata --no-deps --format-version 1
git diff --check
```

For Java movement:

```powershell
python tools\makepad_fork_format.py --changed --check
python tools\check_morphospace_makepad_guards.py
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
