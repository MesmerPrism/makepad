package dev.makepad.android;

import android.os.SystemClock;
import android.util.Base64;

import org.json.JSONObject;

import java.io.ByteArrayOutputStream;
import java.io.EOFException;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.InetSocketAddress;
import java.net.Socket;
import java.nio.charset.StandardCharsets;
import java.util.Random;

final class ManifoldH264CommandClient {
    private static final String MANIFOLD_COMMAND_SCHEMA = "rusty.manifold.command.envelope.v1";
    private static final String LEGACY_RUSTY_XR_BROKER_COMMAND_SCHEMA = "rusty.xr.broker.command.v1";
    private static final String MANIFOLD_EVENTS_PATH = "/manifold/v1/events";

    interface Owner {
        boolean isRunning();
        void setCommandSocket(Socket socket);
        void clearCommandSocket(Socket socket);
    }

    private final Owner owner;

    ManifoldH264CommandClient(Owner owner) {
        this.owner = owner;
    }

    JSONObject sendStartCommand(ExternalH264Config config, long videoId) throws Exception {
        Socket socket = new Socket();
        owner.setCommandSocket(socket);
        try {
            socket.connect(
                new InetSocketAddress(config.brokerHost, config.brokerPort),
                config.commandTimeoutMs);
            socket.setSoTimeout(config.commandTimeoutMs);
            InputStream input = socket.getInputStream();
            OutputStream output = socket.getOutputStream();
            String key = Base64.encodeToString(
                ("makepad-external-h264-" + System.nanoTime()).getBytes(StandardCharsets.US_ASCII),
                Base64.NO_WRAP);
            String request =
                "GET " + MANIFOLD_EVENTS_PATH + " HTTP/1.1\r\n" +
                "Host: " + config.brokerHost + ":" + config.brokerPort + "\r\n" +
                "Upgrade: websocket\r\n" +
                "Connection: Upgrade\r\n" +
                "Sec-WebSocket-Version: 13\r\n" +
                "Sec-WebSocket-Key: " + key + "\r\n" +
                "\r\n";
            output.write(request.getBytes(StandardCharsets.US_ASCII));
            output.flush();
            String status = readHttpLine(input);
            if (status == null || !status.contains("101")) {
                throw new IllegalStateException("Broker WebSocket upgrade failed: " + status);
            }
            while (true) {
                String line = readHttpLine(input);
                if (line == null || line.length() == 0) {
                    break;
                }
            }

            readWebSocketTextFrame(input);
            sendMaskedTextFrame(output, startCommandJson(config, videoId).toString());
            long deadline = SystemClock.elapsedRealtimeNanos() + (long) config.commandTimeoutMs * 1_000_000L;
            while (owner.isRunning() && SystemClock.elapsedRealtimeNanos() < deadline) {
                String text = readWebSocketTextFrame(input);
                if (text == null || text.length() == 0) {
                    continue;
                }
                JSONObject message = new JSONObject(text);
                if ("command_ack".equals(message.optString("type", ""))) {
                    return message;
                }
            }

            throw new IllegalStateException("Timed out waiting for external H.264 command ack.");
        } finally {
            closeQuietly(socket);
            owner.clearCommandSocket(socket);
        }
    }

    private static JSONObject startCommandJson(ExternalH264Config config, long videoId) throws Exception {
        String sourceMode = ExternalH264Config.normalizeSourceMode(config.sourceMode);
        String projectionGeometryProfile =
            ExternalH264Config.projectionGeometryProfileForSource(
                sourceMode,
                config.syntheticProjectionProfile);
        JSONObject params = new JSONObject();
        params.put("device_port", config.streamPort);
        params.put("host_port", config.streamPort);
        params.put("preferred_width", config.preferredWidth);
        params.put("preferred_height", config.preferredHeight);
        params.put("content_width", config.preferredWidth);
        params.put("content_height", config.preferredHeight);
        params.put(
            "desired_display_aspect_ratio",
            config.preferredHeight > 0
                ? (double) config.preferredWidth / (double) config.preferredHeight
                : 1.0);
        params.put("capture_ms", config.captureMs);
        params.put("max_packets", config.maxPackets);
        params.put("bitrate_bps", config.bitrateBps);
        params.put("frame_rate_hz", config.frameRateHz);
        params.put("live_stream", config.liveStream);
        params.put("projection_geometry_profile", projectionGeometryProfile);
        params.put("projectionGeometryProfile", projectionGeometryProfile);
        String sourceSamplingMode = ExternalH264Config.normalizeSourceSamplingMode(config.sourceSamplingMode);
        if (sourceSamplingMode.length() > 0) {
            params.put("source_sampling_mode", sourceSamplingMode);
            params.put("sourceSamplingMode", sourceSamplingMode);
        }
        if (config.targetScreenUvRect.length() > 0) {
            params.put("target_screen_uv_rect", config.targetScreenUvRect);
            params.put("targetScreenUvRect", config.targetScreenUvRect);
        }
        if (config.stereoPairId.length() > 0 && config.stereoPairRole.length() > 0) {
            params.put("stereo_pair_release", true);
            params.put("stereo_pair_id", config.stereoPairId);
            params.put("stereoPairId", config.stereoPairId);
            params.put("stereo_pair_role", config.stereoPairRole);
            params.put("stereoPairRole", config.stereoPairRole);
            params.put("stereo_pair_max_delta_ns", config.stereoPairMaxDeltaNs);
            params.put("stereoPairMaxDeltaNs", config.stereoPairMaxDeltaNs);
        }
        if ("broker-synthetic".equals(sourceMode)) {
            params.put("source_mode", "synthetic_surface");
            params.put("synthetic_pattern", ExternalH264Config.normalizeSyntheticPattern(config.syntheticPattern));
            params.put(
                "synthetic_projection_profile",
                ExternalH264Config.normalizeSyntheticProjectionProfile(config.syntheticProjectionProfile));
        }
        params.put("camera_id", config.cameraId);

        JSONObject command = new JSONObject();
        command.put("type", "command");
        command.put("schema", MANIFOLD_COMMAND_SCHEMA);
        command.put("legacy_schema", LEGACY_RUSTY_XR_BROKER_COMMAND_SCHEMA);
        String clientLabel = config.stereoPairRole.length() > 0
            ? config.stereoPairRole
            : Long.toString(videoId);
        command.put(
            "request_id",
            "makepad-h264-video-" + clientLabel + "-" + videoId + "-" + System.currentTimeMillis());
        command.put(
            "command",
            "broker-synthetic".equals(sourceMode)
                ? "media.start_synthetic_h264_stream"
                : "camera_provider.start_app_camera_h264_stream");
        command.put("client_id", "makepad-external-h264-video-" + clientLabel);
        command.put("app_label", "Makepad XR app");
        command.put("app_version", "source-example");
        command.put("params", params);
        return command;
    }

    private static void sendMaskedTextFrame(OutputStream output, String text) throws Exception {
        byte[] payload = text.getBytes(StandardCharsets.UTF_8);
        output.write(0x81);
        if (payload.length < 126) {
            output.write(0x80 | payload.length);
        } else if (payload.length <= 65535) {
            output.write(0x80 | 126);
            output.write((payload.length >>> 8) & 0xff);
            output.write(payload.length & 0xff);
        } else {
            output.write(0x80 | 127);
            long length = payload.length;
            for (int i = 7; i >= 0; i--) {
                output.write((int) ((length >>> (i * 8)) & 0xff));
            }
        }

        byte[] mask = new byte[4];
        new Random(System.nanoTime()).nextBytes(mask);
        output.write(mask);
        for (int i = 0; i < payload.length; i++) {
            output.write(payload[i] ^ mask[i % 4]);
        }
        output.flush();
    }

    private static String readWebSocketTextFrame(InputStream input) throws Exception {
        int first = input.read();
        if (first < 0) {
            return "";
        }
        int second = input.read();
        if (second < 0) {
            return "";
        }
        int opcode = first & 0x0f;
        boolean masked = (second & 0x80) != 0;
        long length = second & 0x7f;
        if (length == 126) {
            length = readUnsignedShort(input);
        } else if (length == 127) {
            length = readLong(input);
        }
        if (length < 0 || length > 1024 * 1024) {
            throw new IllegalStateException("Broker WebSocket frame is too large.");
        }
        byte[] mask = null;
        if (masked) {
            mask = readExact(input, 4);
        }
        byte[] payload = readExact(input, (int) length);
        if (mask != null) {
            for (int i = 0; i < payload.length; i++) {
                payload[i] = (byte) (payload[i] ^ mask[i % 4]);
            }
        }
        return opcode == 1 ? new String(payload, StandardCharsets.UTF_8) : "";
    }

    private static String readHttpLine(InputStream input) throws Exception {
        ByteArrayOutputStream buffer = new ByteArrayOutputStream();
        int previous = -1;
        while (true) {
            int value = input.read();
            if (value < 0) {
                break;
            }
            if (previous == '\r' && value == '\n') {
                break;
            }
            buffer.write(value);
            previous = value;
            if (buffer.size() > 8192) {
                throw new IllegalStateException("HTTP line exceeded 8192 bytes.");
            }
        }
        byte[] bytes = buffer.toByteArray();
        int length = bytes.length;
        if (length > 0 && bytes[length - 1] == '\r') {
            length--;
        }
        return new String(bytes, 0, length, StandardCharsets.US_ASCII);
    }

    private static int readUnsignedShort(InputStream input) throws Exception {
        int high = input.read();
        int low = input.read();
        if (high < 0 || low < 0) {
            throw new EOFException("Unexpected EOF while reading WebSocket length.");
        }
        return (high << 8) | low;
    }

    private static long readLong(InputStream input) throws Exception {
        byte[] bytes = readExact(input, 8);
        long value = 0L;
        for (int i = 0; i < 8; i++) {
            value = (value << 8) | (bytes[i] & 0xffL);
        }
        return value;
    }

    private static byte[] readExact(InputStream input, int length) throws Exception {
        byte[] bytes = new byte[length];
        int offset = 0;
        while (offset < length) {
            int read = input.read(bytes, offset, length - offset);
            if (read < 0) {
                throw new EOFException("Unexpected EOF while reading payload.");
            }
            offset += read;
        }
        return bytes;
    }

    private static void closeQuietly(Socket socket) {
        if (socket != null) {
            try {
                socket.close();
            } catch (Exception ignored) {
            }
        }
    }
}
