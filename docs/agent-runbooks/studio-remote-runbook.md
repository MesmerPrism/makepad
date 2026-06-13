# Studio Remote Runbook

Use this runbook when a Makepad UI program must be launched, inspected, screenshotted, clicked, or typed through the Studio remote protocol.

## Contents

- Execution policy
- Studio remote startup assumptions
- JSON Lines request protocol
- StudioToApp API notes
- Recommended control flow
- RunItem launch and input reliability notes

---

## Execution Policy
- Visual UI programs must be launched and controlled through the Makepad Studio remote protocol.
- Always use release builds for runtime validation, profiling, benchmarks, timing checks, or any performance-sensitive command. Use `--release` unless the user explicitly asks for a debug build.
- Do not use mount observation or runnable discovery from the bridge client. The bridge must not claim mount ownership from Studio desktop.
- Do not launch UI programs with raw `cargo run`, `cargo makepad`, or ad hoc cargo invocation when a runnable item exists.
- Do not use bridge `Cargo` requests to run applications. Only launch apps from runnable items via bridge `RunItem`.
- For UI runnable targets, do not prebuild or precheck the app from the shell before launching it in Studio. Let the Studio `RunItem` build be the single build path so Cargo fingerprints, env vars, target dirs, and flags stay identical.
- Before starting a new UI run for the same target, send `ClearBuild` for the previous build so Studio stops it and removes its run/log/profiler tabs.
- `cargo check` or `cargo build` never counts as UI verification. After changing UI/runtime code, you must clear the old build and start a fresh Studio run before trusting screenshots, widget dumps, or interaction results.
- Do not keep inspecting an older already-running app after code changes. Re-run the target and verify against the new `build_id`.
- Command-line-only tasks (builds, tests, linting, file ops, grep/ripgrep, etc.) can be run directly in the shell.
- Prefer studio remote control for any workflow that needs screenshots, widget queries, clicks, typing, or runtime UI inspection.
- Before using Studio protocol tools (`FindInFiles`, `ReadTextRange`, `WidgetTreeDump`, `WidgetQuery`, `Screenshot`, `Click`, `TypeText`, `Return`), always start one persistent Studio remote bridge process and reuse it for the entire interaction.

## Assumptions
- Studio is started manually by the user.
- Studio remote target is `ip:port` only (no `http://`, no `ws://`), normally `127.0.0.1:8001`.
  - Use `127.0.0.1:8002` only if Studio reports fallback because `8001` is occupied.
- Keep one persistent studio remote process for the whole interaction.

## Start Studio Remote
- Command:
  - `target/release/cargo-makepad studio --studio=127.0.0.1:8001`
- Send newline-delimited JSON requests on stdin.
- Read newline-delimited JSON responses on stdout.
- Protocol shape is raw `ClientToHub` requests on stdin and filtered `HubToClient` responses on stdout.
- Do not send `ObserveMount` from the bridge. It can take `primary` UI ownership for the mount and divert RunView/framebuffer traffic away from Studio desktop.

## Request Protocol (JSON Lines)
- `{"ListBuilds":[]}`
- `{"ClearBuild":{"build_id":[6]}}` stops a running build and immediately clears its Studio UI tabs; use this before rerunning the same app.
- `{"StopBuild":{"build_id":[6]}}` stops/kills a running build but does not clear Studio tabs.
- `{"RunItem":{"mount":"makepad","name":"makepad-example-todo"}}`
- `{"RunItem":{"mount":"makepad","name":"makepad-example-xr-quest"}}`
- `{"FindInFiles":{"mount":"makepad","pattern":"ClientToHub::","is_regex":false,"glob":null,"max_results":200}}`
- `{"FindInFiles":{"mount":"makepad","pattern":"ClientToHub::(FindInFiles|ReadTextRange)","is_regex":true,"glob":"**/*.rs","max_results":200}}`
- `{"ReadTextRange":{"path":"makepad/studio/backend/src/dispatch.rs","start_line":640,"end_line":720}}`
- `{"WidgetTreeDump":{"build_id":[6]}}`
- `{"WidgetQuery":{"build_id":[6],"query":"id:todo_input"}}`
- `{"Screenshot":{"build_id":[6],"kind_id":0}}` (`kind_id` optional; defaults to `0`)
- `{"Click":{"build_id":[6],"x":1274,"y":342}}`
- `{"TypeText":{"build_id":[6],"text":"hello"}}`
- `{"Return":{"build_id":[6],"auto_dump":false}}`
- `{"ForwardToApp":{"build_id":[6],"msg_bin":[...]}}` (advanced; binary payload)

## `StudioToApp` API (Updated)
- The studio remote bridge supports raw app event passthrough via `UIToStudio::ForwardToApp`.
- Current `StudioToApp` variants include:
  - `Screenshot`, `WidgetTreeDump`, `KeepAlive`, `LiveChange`, `Swapchain`, `WindowGeomChange`, `Tick`
  - `MouseDown`, `MouseUp`, `MouseMove`, `Scroll`
  - `KeyDown`, `KeyUp`, `TextInput`, `TextCopy`, `TextCut`
  - `None`, `Kill`
- Use direct studio remote requests (`Screenshot`, `WidgetTreeDump`, `Click`, `TypeText`, `Return`) for normal automation.
- Use raw `StudioToApp` only for low-level event injection/debugging.

## Response Notes (Current)
- Bridge stdout is filtered to: `Hello`, `Error`, `TextFileRead`, `TextFileRange`, `FindFileResults`, `SearchFileResults`, `Builds`, `RunItems`, `BuildStarted`, `BuildStopped`, `BuildCleared`, `AppStarted`, `RunViewCreated`, `QueryLogResults`, `Screenshot`, `WidgetTreeDump`, `WidgetQuery`, `QueryCancelled`.
- `BuildCleared` is a Studio frontend cleanup signal routed to the primary UI for the build's mount; bridge clients should not wait for it before starting the next run.
- `RunViewFrame` and the terminal stream are not exposed by the bridge.
- `Screenshot` responses include file metadata (`path`, `width`, `height`) and not inline PNG bytes.
- `WidgetTreeDump` responses include text dump content keyed by `request_id`.
- `FindInFiles` responds as `SearchFileResults` with concise entries (`path`, `line`, `column`, `line_text`) and `done`.
- `FindInFiles` defaults to searching only `.rs`, `.md`, `.toml` files unless `glob` is provided.
- `ReadTextRange` responds as `TextFileRange` with `path`, requested `start_line`/`end_line`, `total_lines`, and `content`.
- Query-scoped responses are lane-filtered by `query_id.client_id`; only this bridge client's query results are emitted.
- Build ids and query ids are `QueryId` tuple structs, so JSON encodes them as one-element arrays like `[6]`.
- `FindInFiles`/`SearchFiles` execution is worker-pooled in backend (not main dispatch thread).

## Recommended Control Flow
1. Start studio remote process once.
2. Determine the target runnable item name locally from the repo or from the user request.
3. Call `ListBuilds` and find any existing build for the same runnable item.
4. Send `ClearBuild` for that old `build_id`; do not wait for an acknowledgment before the next launch.
5. Start the new UI app through `RunItem`, and wait for `BuildStarted` and `AppStarted`.
6. After any code change that affects runtime/UI behavior, repeat steps 3-5 before doing screenshots, widget dumps, clicks, or visual conclusions.
7. For code search, use `FindInFiles` first, then `ReadTextRange` to window exact regions.
8. Use direct shell cargo commands for non-launch tasks such as `check`, `build`, `test`, or `bench`.
9. Use `WidgetQuery` / `WidgetTreeDump` to get click targets.
10. For text input, click field first, then send text, then return.
11. Keep control packets compact (`auto_dump:false` on click/type/return for low latency).

## `RunItem` Launch
- `RunItem` executes a Studio-defined runnable item by name.
- Use the runnable item name shown in Studio, not a Cargo package name.
- `RunItem` does not implicitly replace an older build tab; agents should clear the old build themselves first with `ClearBuild`.

## One-Flow Input Burst
- Send this as one stdin write (multiple JSON lines, no sleeps):
  - `Click` (input field center)
  - `TypeText`
  - `Return`
- Then request `WidgetTreeDump` or `Screenshot` to confirm.

## Coordinates
- Use coordinates from dump as-is.
- `W3` dump uses integer pixel coordinates in the same space expected by `Click`.
- Do not apply extra DPI math in the agent loop.

## Reliability Notes
- `Screenshot` can arrive before visible redraw after rapid input bursts.
  - If screenshot looks stale, request a follow-up `WidgetTreeDump`/`Screenshot`.
- If input does nothing:
  - Verify `build_id` with `ListBuilds`.
  - Refresh dump and retry click on input before typing.
- If request errors with no active websocket:
  - app is not connected yet; wait for startup completion and retry.
