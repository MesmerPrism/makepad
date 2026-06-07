package dev.makepad.android;

import java.util.Locale;

class ExternalH264Config {
    static final int MAX_STREAM_PACKETS = 2400;
    static final String DEFAULT_CAMERA_PROJECTION_GEOMETRY_PROFILE = "full-frame-diagnostic";
    static final String CAMERA_PROJECTION_GEOMETRY_PROFILE = "camera-projection";
    static final String DECODE_OUTPUT_AUTO = "auto";
    static final String DECODE_OUTPUT_CPU_YUV = "cpu-yuv";
    static final String DECODE_OUTPUT_SURFACE_TEXTURE = "surface-texture";
    static final String DECODE_OUTPUT_HARDWARE_BUFFER = "hardware-buffer";
    static final String SOURCE_SAMPLING_TARGET_LOCAL_RASTER = "target-local-raster";
    static final String SOURCE_SAMPLING_SCREEN_TO_CAMERA_HOMOGRAPHY = "screen-to-camera-homography";

    final String brokerHost;
    final int brokerPort;
    final int streamPort;
    final String sourceMode;
    final String decodeOutputMode;
    final String syntheticPattern;
    final String syntheticProjectionProfile;
    final String sourceSamplingMode;
    final String targetScreenUvRect;
    final String cameraId;
    final String stereoPairId;
    final String stereoPairRole;
    final int stereoPairMaxDeltaNs;
    final int preferredWidth;
    final int preferredHeight;
    final int captureMs;
    final int maxPackets;
    final int bitrateBps;
    final int frameRateHz;
    final int commandTimeoutMs;
    final int streamTimeoutMs;
    final int decodeTimeoutMs;
    final boolean liveStream;

    ExternalH264Config(
        String brokerHost,
        int brokerPort,
        int streamPort,
        String sourceMode,
        String decodeOutputMode,
        String syntheticPattern,
        String syntheticProjectionProfile,
        String sourceSamplingMode,
        String targetScreenUvRect,
        String cameraId,
        String stereoPairId,
        String stereoPairRole,
        int stereoPairMaxDeltaNs,
        int preferredWidth,
        int preferredHeight,
        int captureMs,
        int maxPackets,
        int bitrateBps,
        int frameRateHz,
        int commandTimeoutMs,
        int streamTimeoutMs,
        int decodeTimeoutMs,
        boolean liveStream) {
        this.brokerHost = brokerHost != null && brokerHost.trim().length() > 0
            ? brokerHost
            : "127.0.0.1";
        this.brokerPort = clamp(brokerPort, 1, 65535);
        this.streamPort = clamp(streamPort, 1, 65535);
        this.sourceMode = normalizeSourceMode(sourceMode);
        this.decodeOutputMode = normalizeDecodeOutputMode(decodeOutputMode);
        this.syntheticPattern = normalizeSyntheticPattern(syntheticPattern);
        this.syntheticProjectionProfile = "broker-camera".equals(this.sourceMode)
            ? normalizeCameraProjectionGeometryProfile(syntheticProjectionProfile)
            : normalizeSyntheticProjectionProfile(syntheticProjectionProfile);
        this.sourceSamplingMode = normalizeSourceSamplingMode(sourceSamplingMode);
        this.targetScreenUvRect = targetScreenUvRect != null ? targetScreenUvRect.trim() : "";
        this.cameraId = cameraId != null ? cameraId.trim() : "";
        this.stereoPairId = stereoPairId != null ? stereoPairId.trim() : "";
        this.stereoPairRole = normalizeStereoPairRole(stereoPairRole);
        this.stereoPairMaxDeltaNs = clamp(stereoPairMaxDeltaNs, 0, 250000000);
        this.preferredWidth = clamp(preferredWidth, 16, 4096);
        this.preferredHeight = clamp(preferredHeight, 16, 4096);
        this.captureMs = captureMs <= 0 ? 0 : clamp(captureMs, 100, 120000);
        this.maxPackets = clamp(maxPackets, 0, MAX_STREAM_PACKETS);
        this.bitrateBps = clamp(bitrateBps, 100000, 20000000);
        this.frameRateHz = clamp(frameRateHz, 1, 120);
        this.commandTimeoutMs = clamp(commandTimeoutMs, 500, 60000);
        this.streamTimeoutMs = clamp(streamTimeoutMs, 500, 120000);
        this.decodeTimeoutMs = clamp(decodeTimeoutMs, 500, 60000);
        this.liveStream = liveStream;
    }

    ExternalH264Config(ExternalH264Config source) {
        this(
            source.brokerHost,
            source.brokerPort,
            source.streamPort,
            source.sourceMode,
            source.decodeOutputMode,
            source.syntheticPattern,
            source.syntheticProjectionProfile,
            source.sourceSamplingMode,
            source.targetScreenUvRect,
            source.cameraId,
            source.stereoPairId,
            source.stereoPairRole,
            source.stereoPairMaxDeltaNs,
            source.preferredWidth,
            source.preferredHeight,
            source.captureMs,
            source.maxPackets,
            source.bitrateBps,
            source.frameRateHz,
            source.commandTimeoutMs,
            source.streamTimeoutMs,
            source.decodeTimeoutMs,
            source.liveStream);
    }

    boolean isUnboundedLiveStream() {
        return liveStream && captureMs == 0 && maxPackets == 0;
    }

    boolean usesStereoHardwareBufferPairing() {
        return stereoPairId.length() > 0 &&
            ("left".equals(stereoPairRole) || "right".equals(stereoPairRole));
    }

    static ExternalH264Config defaults() {
        return new ExternalH264Config(
            "127.0.0.1",
            8765,
            8879,
            "broker-synthetic",
            DECODE_OUTPUT_AUTO,
            "diagnostic-grid",
            "head-anchored-virtual-camera",
            "",
            "",
            "",
            "",
            "",
            25000000,
            1280,
            1280,
            900,
            32,
            2000000,
            30,
            10000,
            20000,
            5000,
            false);
    }

    static String normalizeSourceMode(String value) {
        if (value == null || value.trim().length() == 0) {
            return "broker-synthetic";
        }
        String normalized = value.trim().toLowerCase(Locale.US).replace('_', '-');
        if ("existing".equals(normalized) ||
            "existing-stream".equals(normalized) ||
            "remote".equals(normalized) ||
            "proxied".equals(normalized) ||
            "proxy".equals(normalized) ||
            "proxy-stream".equals(normalized) ||
            "incoming".equals(normalized) ||
            "incoming-stream".equals(normalized)) {
            return "existing-stream";
        }
        if ("camera".equals(normalized) ||
            "broker-camera".equals(normalized) ||
            "camera2".equals(normalized)) {
            return "broker-camera";
        }
        return "broker-synthetic";
    }

    static String normalizeDecodeOutputMode(String value) {
        if (value == null || value.trim().length() == 0) {
            return DECODE_OUTPUT_AUTO;
        }
        String normalized = value.trim().toLowerCase(Locale.US).replace('_', '-');
        if ("auto".equals(normalized) || "default".equals(normalized)) {
            return DECODE_OUTPUT_AUTO;
        }
        if ("cpu".equals(normalized) ||
            "yuv".equals(normalized) ||
            "cpu-yuv".equals(normalized) ||
            "software-yuv".equals(normalized)) {
            return DECODE_OUTPUT_CPU_YUV;
        }
        if ("hwb".equals(normalized) ||
            "hardware-buffer".equals(normalized) ||
            "hardware-buffer-external".equals(normalized) ||
            "image-reader".equals(normalized) ||
            "imagereader".equals(normalized)) {
            return DECODE_OUTPUT_HARDWARE_BUFFER;
        }
        if ("oes".equals(normalized) ||
            "external-oes".equals(normalized) ||
            "surface".equals(normalized) ||
            "surface-texture".equals(normalized) ||
            "surfacetexture".equals(normalized)) {
            return DECODE_OUTPUT_SURFACE_TEXTURE;
        }
        return DECODE_OUTPUT_AUTO;
    }

    static String normalizeStereoPairRole(String value) {
        if (value == null) {
            return "";
        }
        String normalized = value.trim().toLowerCase(Locale.US).replace('_', '-');
        if ("left".equals(normalized) || "l".equals(normalized) || "0".equals(normalized)) {
            return "left";
        }
        if ("right".equals(normalized) || "r".equals(normalized) || "1".equals(normalized)) {
            return "right";
        }
        return "";
    }

    static String normalizeSyntheticPattern(String value) {
        if (value == null || value.trim().length() == 0) {
            return "diagnostic-grid";
        }
        String normalized = value.trim().toLowerCase(Locale.US).replace('_', '-');
        if ("checker".equals(normalized) || "checkerboard".equals(normalized)) {
            return "checkerboard";
        }
        if ("ramp".equals(normalized) || "luma".equals(normalized) || "luma-ramp".equals(normalized)) {
            return "luma-ramp";
        }
        if ("motion".equals(normalized) || "motion-bar".equals(normalized)) {
            return "motion-bar";
        }
        return "diagnostic-grid";
    }

    static String normalizeSyntheticProjectionProfile(String value) {
        if (value == null || value.trim().length() == 0) {
            return "head-anchored-virtual-camera";
        }
        String normalized = value.trim().toLowerCase(Locale.US).replace('_', '-');
        if ("camera-matched".equals(normalized) || "camera-matched-synthetic".equals(normalized)) {
            return "camera-matched";
        }
        if ("full-frame".equals(normalized) ||
                "full-frame-diagnostic".equals(normalized) ||
                "projection-space-diagnostic".equals(normalized)) {
            return "full-frame-diagnostic";
        }
        if ("head-anchored-virtual-camera".equals(normalized)) {
            return "head-anchored-virtual-camera";
        }
        return "head-anchored-virtual-camera";
    }

    static String normalizeCameraProjectionGeometryProfile(String value) {
        if (value == null || value.trim().length() == 0) {
            return DEFAULT_CAMERA_PROJECTION_GEOMETRY_PROFILE;
        }
        String normalized = value.trim().toLowerCase(Locale.US).replace('_', '-');
        if ("full-frame".equals(normalized) ||
                DEFAULT_CAMERA_PROJECTION_GEOMETRY_PROFILE.equals(normalized) ||
                "projection-space-diagnostic".equals(normalized)) {
            return DEFAULT_CAMERA_PROJECTION_GEOMETRY_PROFILE;
        }
        if (CAMERA_PROJECTION_GEOMETRY_PROFILE.equals(normalized) ||
                "camera-footprint".equals(normalized) ||
                "camera-projection-footprint".equals(normalized)) {
            return CAMERA_PROJECTION_GEOMETRY_PROFILE;
        }
        throw new IllegalArgumentException(
            "Unsupported broker camera projection geometry profile: " + value);
    }

    static String projectionGeometryProfileForSource(String sourceMode, String value) {
        if ("broker-camera".equals(normalizeSourceMode(sourceMode))) {
            return normalizeCameraProjectionGeometryProfile(value);
        }
        return normalizeSyntheticProjectionProfile(value);
    }

    static String normalizeSourceSamplingMode(String value) {
        if (value == null || value.trim().length() == 0) {
            return "";
        }
        String normalized = value.trim().toLowerCase(Locale.US).replace('_', '-');
        if ("target-local-raster".equals(normalized) ||
                "target-local".equals(normalized) ||
                "target-raster".equals(normalized) ||
                "local-raster".equals(normalized) ||
                "raster".equals(normalized)) {
            return SOURCE_SAMPLING_TARGET_LOCAL_RASTER;
        }
        if ("screen-to-camera-homography".equals(normalized) ||
                "screen-camera-homography".equals(normalized) ||
                "screen-to-source-homography".equals(normalized) ||
                "camera-homography".equals(normalized) ||
                "camera-projection".equals(normalized) ||
                "homography".equals(normalized)) {
            return SOURCE_SAMPLING_SCREEN_TO_CAMERA_HOMOGRAPHY;
        }
        throw new IllegalArgumentException("Unsupported external H.264 source sampling mode: " + value);
    }

    private static int clamp(int value, int min, int max) {
        return Math.max(min, Math.min(max, value));
    }
}
