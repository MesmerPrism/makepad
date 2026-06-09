# Morphospace Makepad Marker Boundary Map

This branch still contains historical Rusty XR marker strings because it is a
maintained Makepad fork for the public Morphospace Makepad Quest lane. Treat
these strings as compatibility evidence unless this file says they are active
defaults.

Do not broadly rename marker text as cleanup. Rename only when a behavior slice
touches the owner and the downstream evidence tools are updated in the same
slice.

## Classification

| Surface | Examples | Classification | Rule |
| --- | --- | --- | --- |
| Manifold command defaults | `rusty.manifold.command.envelope.v1`, `/manifold/v1/events` | active default | Keep as the default command/session lane. Do not replace with old Rusty XR broker route names. |
| Manifold stream framing | `RMANVID1` | active default | Keep as the default external H.264 stream magic. |
| Old broker command alias | `LEGACY_RUSTY_XR_BROKER_COMMAND_SCHEMA = "rusty.xr.broker.command.v1"` | explicit legacy alias | Keep only behind `LEGACY_*` or compatibility field names. Never use as the emitted default. |
| Old stream magic alias | `LEGACY_STREAM_MAGIC = "RXYRVID1"` | explicit legacy alias | Keep only as a read compatibility path. New writers should emit `RMANVID1`. |
| Makepad diagnostic schemas | `schema=rusty.xr.makepad-*` | historical diagnostic evidence | Keep until the owning diagnostic family is touched. New marker families should prefer `rusty.makepad.*`, `rusty.quest.makepad.*`, or a Manifold name only when Manifold is the authority. |
| Android debug properties | `debug.rustyxr.*` | compatibility property alias | Keep read/cleanup compatibility. New defaults should be owner-neutral or app/Quest/Manifold-owned when that route is next edited. |
| MediaProjection launch extras | `rustyxr.mediaProjection*` | compatibility launch alias | Keep as historical activity-shell input until the public Rusty XR wrapper migrates. Do not treat it as Manifold command authority. |

## Runtime Marker Decisions

Do not rename these markers as cleanup. Use this table only when a behavior
slice is already touching the owner, and update downstream evidence tools in the
same slice.

| Runtime marker family | Keep compatibility | Rename-on-touch | Retire | Replace with owner |
| --- | --- | --- | --- | --- |
| `debug.rustyxr.xr.display.refresh.rate.hz` | yes | yes; keep a compatibility read path if legacy/public Rusty XR wrappers still set it | no immediate retire | Quest/Makepad-owned Android display-refresh property, not Manifold authority |
| `rusty.xr.makepad-android-bootstrap.v1` and `rusty.xr.makepad-android-activity.v1` | yes | yes | no immediate retire | Makepad-owned Android app-shell phase marker |
| `rusty.xr.makepad-camera-frame-flow.v1` | yes | yes | retire duplicate phases only after downstream frame-flow evidence no longer consumes them | Quest/Makepad-owned camera frame-flow diagnostic |
| `rusty.xr.makepad-broker-h264-*` | yes | yes; replace `broker` wording only in the touched H.264 stream/slot slice | no immediate retire | Makepad-owned H.264 diagnostic, or Manifold-owned only if the event becomes a Manifold contract |
| `rusty.xr.makepad-direct-stereo-hardware-buffer-*` | yes | yes | no immediate retire | Makepad-owned hardware-buffer stereo diagnostic |
| `rusty.xr.makepad-openxr-*` | yes | yes | no immediate retire | Makepad-owned OpenXR frame diagnostic |
| `rusty.xr.makepad-vulkan-video-*` and `rusty.xr.makepad-vulkan-resource-retire.v1` | yes | yes | retire issue-specific color/descriptor markers after the Vulkan evidence gate stops reading them | Makepad-owned Vulkan video/resource diagnostic |

## Rename-On-Touch Queue

Use this queue when an affected behavior slice is already editing the owner:

1. Vulkan hardware-buffer diagnostic schemas in `platform/src/os/linux/vulkan.rs`.
2. Android direct/stereo H.264 diagnostic schemas in `platform/src/os/linux/android/android.rs`.
3. JNI latest-slot diagnostic schemas in `platform/src/os/linux/android/android_jni.rs`.
4. Camera frame-flow schemas in `platform/src/os/linux/android/android_camera_player.rs`.
5. Activity bootstrap marker schema in `RustyXrActivitySupport.java`.
6. MediaProjection launch extras in `RustyXrMediaProjectionHelper.java`.
7. Display refresh debug property in `platform/src/os/linux/android/android.rs`.

Each rename-on-touch slice needs a downstream evidence-tool scan before commit.
If a marker is still consumed by legacy/public Rusty XR validation tools, either keep a
compatibility alias or update the validator and this map together.

## Guard

Run:

```powershell
python tools\check_morphospace_makepad_guards.py
```

This checks the active Manifold H.264 defaults, explicit legacy aliases, stale
doc pointers, split helper files, and the presence of this runtime marker
decision table.
