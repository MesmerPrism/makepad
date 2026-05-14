package dev.makepad.android;

import android.app.Activity;
import android.graphics.SurfaceTexture;
import android.media.MediaCodec;
import android.media.MediaCodecInfo;
import android.media.MediaFormat;
import android.os.Bundle;
import android.os.SystemClock;
import android.util.Base64;
import android.util.Log;
import android.view.Surface;

import org.json.JSONObject;

import java.io.ByteArrayOutputStream;
import java.io.DataInputStream;
import java.io.EOFException;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.InetSocketAddress;
import java.net.Socket;
import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;
import java.util.Locale;
import java.util.Random;
import java.util.concurrent.atomic.AtomicBoolean;

final class BrokerH264VideoPlayer extends VideoPlayer {
    private static final String TAG = "MakepadBrokerH264";
    private static final String STREAM_MAGIC = "RXYRVID1";
    private static final int CODEC_H264 = 1;
    private static final int MAX_PACKET_BYTES = 1024 * 1024;
    private static final int MAX_STREAM_HEADER_METADATA_BYTES = 256 * 1024;
    private static final int MAX_STREAM_PACKETS = 2400;
    private static final int DEQUEUE_TIMEOUT_US = 10000;

    private final Config mConfig;
    private final AtomicBoolean mStarted = new AtomicBoolean(false);
    private volatile boolean mRunning = true;
    private volatile Socket mBrokerSocket;
    private volatile Socket mStreamSocket;
    private volatile MediaCodec mDecoder;
    private Surface mDecodeSurface;
    private Thread mDecodeThread;

    BrokerH264VideoPlayer(Activity activity, long videoId, Config config) {
        super(activity, videoId);
        mConfig = config != null ? config : Config.defaults();
    }

    @Override
    public void prepareVideoPlayback() {
        try {
            mSurfaceTexture = new SurfaceTexture(mExternalTextureHandle);
            mSurfaceTexture.setDefaultBufferSize(
                Math.max(1, mConfig.preferredWidth),
                Math.max(1, mConfig.preferredHeight));

            mHandlerThread = new android.os.HandlerThread("BrokerH264SurfaceTexture");
            mHandlerThread.start();
            mGlHandler = new android.os.Handler(mHandlerThread.getLooper());
            mSurfaceTexture.setOnFrameAvailableListener(surfaceTexture -> {
                mAvailableFrames.incrementAndGet();
            }, mGlHandler);

            mDecodeSurface = new Surface(mSurfaceTexture);
            mIsPrepared = true;
            if (mAutoplay) {
                beginPlayback();
            }
        } catch (Exception ex) {
            MakepadNative.onVideoDecodingError(mVideoId, safeMessage(ex));
        }
    }

    @Override
    public void beginPlayback() {
        if (!mIsPrepared || !mStarted.compareAndSet(false, true)) {
            return;
        }
        mRunning = true;
        mDecodeThread = new Thread(this::runDecode, "MakepadBrokerH264Decode");
        mDecodeThread.start();
    }

    @Override
    public void pausePlayback() {
    }

    @Override
    public void resumePlayback() {
        beginPlayback();
    }

    @Override
    public void stopAndCleanup() {
        mRunning = false;
        closeQuietly(mBrokerSocket);
        closeQuietly(mStreamSocket);
        MediaCodec decoder = mDecoder;
        if (decoder != null) {
            try {
                decoder.stop();
            } catch (Exception ignored) {
            }
        }
        if (mDecodeThread != null) {
            try {
                mDecodeThread.join(1000);
            } catch (InterruptedException ex) {
                Thread.currentThread().interrupt();
            }
            mDecodeThread = null;
        }
        if (mDecodeSurface != null) {
            try {
                mDecodeSurface.release();
            } catch (RuntimeException ignored) {
            }
            mDecodeSurface = null;
        }
        super.stopAndCleanup();
    }

    private void runDecode() {
        try {
            if (shouldStartBrokerStream()) {
                JSONObject ack = sendStartCommand();
                if (!ack.optBoolean("accepted", false)) {
                    throw new IllegalStateException(
                        "Broker rejected H.264 stream command: " + ack.optString("message", ""));
                }
            }
            decodeStream();
            notifyCompleted();
        } catch (Exception ex) {
            if (mRunning) {
                Log.w(TAG, "Broker H.264 playback failed: " + safeMessage(ex), ex);
                MakepadNative.onVideoDecodingError(mVideoId, safeMessage(ex));
            }
        }
    }

    private boolean shouldStartBrokerStream() {
        return !"existing-stream".equals(normalizeSourceMode(mConfig.sourceMode));
    }

    private JSONObject sendStartCommand() throws Exception {
        Socket socket = new Socket();
        mBrokerSocket = socket;
        socket.connect(
            new InetSocketAddress(mConfig.brokerHost, mConfig.brokerPort),
            mConfig.commandTimeoutMs);
        socket.setSoTimeout(mConfig.commandTimeoutMs);
        InputStream input = socket.getInputStream();
        OutputStream output = socket.getOutputStream();
        String key = Base64.encodeToString(
            ("makepad-broker-h264-" + System.nanoTime()).getBytes(StandardCharsets.US_ASCII),
            Base64.NO_WRAP);
        String request =
            "GET /rustyxr/v1/events HTTP/1.1\r\n" +
            "Host: " + mConfig.brokerHost + ":" + mConfig.brokerPort + "\r\n" +
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
        sendMaskedTextFrame(output, startCommandJson().toString());
        long deadline = SystemClock.elapsedRealtimeNanos() + (long) mConfig.commandTimeoutMs * 1_000_000L;
        while (mRunning && SystemClock.elapsedRealtimeNanos() < deadline) {
            String text = readWebSocketTextFrame(input);
            if (text == null || text.length() == 0) {
                continue;
            }
            JSONObject message = new JSONObject(text);
            if ("command_ack".equals(message.optString("type", ""))) {
                closeQuietly(socket);
                mBrokerSocket = null;
                return message;
            }
        }

        closeQuietly(socket);
        mBrokerSocket = null;
        throw new IllegalStateException("Timed out waiting for broker H.264 command ack.");
    }

    private JSONObject startCommandJson() throws Exception {
        String sourceMode = normalizeSourceMode(mConfig.sourceMode);
        JSONObject params = new JSONObject();
        params.put("device_port", mConfig.streamPort);
        params.put("host_port", mConfig.streamPort);
        params.put("preferred_width", mConfig.preferredWidth);
        params.put("preferred_height", mConfig.preferredHeight);
        params.put("capture_ms", mConfig.captureMs);
        params.put("max_packets", mConfig.maxPackets);
        params.put("bitrate_bps", mConfig.bitrateBps);
        params.put("live_stream", mConfig.liveStream);
        if ("broker-synthetic".equals(sourceMode)) {
            params.put("source_mode", "synthetic_surface");
            params.put("synthetic_pattern", normalizeSyntheticPattern(mConfig.syntheticPattern));
        }

        JSONObject command = new JSONObject();
        command.put("type", "command");
        command.put("schema", "rusty.xr.broker.command.v1");
        command.put("request_id", "makepad-h264-video-" + System.currentTimeMillis());
        command.put(
            "command",
            "broker-synthetic".equals(sourceMode)
                ? "media.start_synthetic_h264_stream"
                : "camera_provider.start_app_camera_h264_stream");
        command.put("client_id", "makepad-broker-h264-video");
        command.put("app_label", "Makepad XR app");
        command.put("app_version", "source-example");
        command.put("params", params);
        return command;
    }

    private void decodeStream() throws Exception {
        Socket socket = connectWithRetry(mConfig.brokerHost, mConfig.streamPort, mConfig.streamTimeoutMs);
        mStreamSocket = socket;
        socket.setSoTimeout(mConfig.streamTimeoutMs);
        DataInputStream input = new DataInputStream(socket.getInputStream());
        StreamHeader header = readHeader(input);
        if (header.codecId != CODEC_H264) {
            throw new IllegalStateException("Broker stream codec is not H.264: " + header.codecId);
        }

        List<Packet> pending = new ArrayList<Packet>();
        int packetsRead = 0;
        while (mRunning &&
            shouldReadMorePrimerPackets(header, pending, packetsRead)) {
            Packet packet = readPacket(input, header.schemaVersion);
            pending.add(packet);
            packetsRead++;
        }

        NalUnit sps = findNalUnit(pending, 7);
        NalUnit pps = findNalUnit(pending, 8);
        boolean hasCompleteCsd = sps != null && pps != null;
        MediaFormat format = MediaFormat.createVideoFormat("video/avc", header.width, header.height);
        if (sps != null) {
            format.setByteBuffer("csd-0", ByteBuffer.wrap(sps.bytes));
        }
        if (pps != null) {
            format.setByteBuffer("csd-1", ByteBuffer.wrap(pps.bytes));
        }
        try {
            format.setInteger(MediaFormat.KEY_LOW_LATENCY, 1);
        } catch (Exception ignored) {
        }

        MediaCodec decoder = MediaCodec.createDecoderByType("video/avc");
        mDecoder = decoder;
        decoder.configure(format, mDecodeSurface, null, 0);
        decoder.start();
        requestDecoderLowLatency(decoder);
        notifyPrepared(header.width, header.height);

        MediaCodec.BufferInfo info = new MediaCodec.BufferInfo();
        int nextPending = 0;
        long lastPtsUs = 0L;
        boolean inputEosQueued = false;
        boolean outputEosSeen = false;
        int decodedFrameCount = 0;
        long deadline = SystemClock.elapsedRealtimeNanos() +
            (long) Math.max(1, mConfig.decodeTimeoutMs + mConfig.streamTimeoutMs + mConfig.captureMs) * 1_000_000L;

        while (mRunning && !outputEosSeen && SystemClock.elapsedRealtimeNanos() < deadline) {
            if (!inputEosQueued) {
                int inputIndex = decoder.dequeueInputBuffer(DEQUEUE_TIMEOUT_US);
                if (inputIndex >= 0) {
                    Packet packet = null;
                    while (nextPending < pending.size() && packet == null) {
                        Packet candidate = pending.get(nextPending++);
                        if (!hasCompleteCsd || !candidate.isCodecConfig()) {
                            packet = candidate;
                        }
                    }
                    if (packet == null && shouldReadMorePackets(header, packetsRead)) {
                        try {
                            packet = readPacket(input, header.schemaVersion);
                            packetsRead++;
                        } catch (EOFException eof) {
                            packet = null;
                        }
                    }
                    if (packet != null) {
                        queuePacket(decoder, inputIndex, packet, hasCompleteCsd);
                        lastPtsUs = packet.ptsUs;
                    } else {
                        decoder.queueInputBuffer(
                            inputIndex,
                            0,
                            0,
                            lastPtsUs,
                            MediaCodec.BUFFER_FLAG_END_OF_STREAM);
                        inputEosQueued = true;
                    }
                }
            }

            int outputIndex = decoder.dequeueOutputBuffer(info, DEQUEUE_TIMEOUT_US);
            if (outputIndex == MediaCodec.INFO_TRY_AGAIN_LATER) {
                continue;
            }
            if (outputIndex == MediaCodec.INFO_OUTPUT_FORMAT_CHANGED) {
                continue;
            }
            if (outputIndex < 0) {
                continue;
            }

            boolean codecConfig = (info.flags & MediaCodec.BUFFER_FLAG_CODEC_CONFIG) != 0;
            boolean eos = (info.flags & MediaCodec.BUFFER_FLAG_END_OF_STREAM) != 0;
            decoder.releaseOutputBuffer(outputIndex, !codecConfig && !eos);
            if (!codecConfig && !eos) {
                decodedFrameCount++;
            }
            outputEosSeen = eos;
        }

        if (decodedFrameCount == 0 && mRunning) {
            throw new IllegalStateException("Broker H.264 decoder produced no output frames.");
        }
    }

    private StreamHeader readHeader(DataInputStream input) throws Exception {
        byte[] magicBytes = new byte[8];
        input.readFully(magicBytes);
        String magic = new String(magicBytes, StandardCharsets.US_ASCII);
        if (!STREAM_MAGIC.equals(magic)) {
            throw new IllegalStateException("Unexpected broker stream magic: " + magic);
        }

        int schemaVersion = input.readInt();
        int codecId = input.readInt();
        int width = input.readInt();
        int height = input.readInt();
        int packetCount = input.readInt();
        int headerMetadataBytes = input.readInt();
        if (schemaVersion < 1 || schemaVersion > 3) {
            throw new IllegalStateException("Unsupported broker stream schema version: " + schemaVersion);
        }
        if (packetCount < 0 || packetCount > MAX_STREAM_PACKETS) {
            throw new IllegalStateException("Broker stream packet count is out of range: " + packetCount);
        }
        if (headerMetadataBytes < 0 || headerMetadataBytes > MAX_STREAM_HEADER_METADATA_BYTES) {
            throw new IllegalStateException("Broker stream metadata header is out of range: " + headerMetadataBytes);
        }
        if (headerMetadataBytes > 0) {
            byte[] ignored = new byte[headerMetadataBytes];
            input.readFully(ignored);
        }
        Log.i(TAG, String.format(
            Locale.US,
            "Broker H.264 stream header videoId=%d schema=%d width=%d height=%d packets=%d",
            mVideoId,
            schemaVersion,
            width,
            height,
            packetCount));
        return new StreamHeader(schemaVersion, codecId, width, height, packetCount);
    }

    private boolean shouldReadMorePrimerPackets(StreamHeader header, List<Packet> pending, int packetsRead) {
        if (header.packetCount > 0 && packetsRead >= header.packetCount) {
            return false;
        }
        if (pending.size() >= 8) {
            return false;
        }
        return findNalUnit(pending, 7) == null || findNalUnit(pending, 8) == null;
    }

    private boolean shouldReadMorePackets(StreamHeader header, int packetsRead) {
        if (header.packetCount > 0) {
            return packetsRead < header.packetCount;
        }
        if (!mConfig.liveStream && mConfig.maxPackets > 0) {
            return packetsRead < mConfig.maxPackets;
        }
        return true;
    }

    private static Packet readPacket(DataInputStream input, int schemaVersion) throws Exception {
        long ptsUs = input.readLong();
        int flags = input.readInt();
        int size = input.readInt();
        if (size < 0 || size > MAX_PACKET_BYTES) {
            throw new IllegalStateException("Broker stream packet size is out of range: " + size);
        }
        if (schemaVersion >= 2) {
            input.readLong();
            input.readLong();
        }
        byte[] payload = new byte[size];
        input.readFully(payload);
        return new Packet(ptsUs, flags, payload);
    }

    private static void queuePacket(
        MediaCodec decoder,
        int inputIndex,
        Packet packet,
        boolean hasCompleteCsd) throws Exception {
        ByteBuffer inputBuffer = decoder.getInputBuffer(inputIndex);
        if (inputBuffer == null) {
            throw new IllegalStateException("Decoder input buffer is unavailable.");
        }
        if (packet.payload.length > inputBuffer.capacity()) {
            throw new IllegalStateException("Encoded packet exceeds decoder input capacity.");
        }
        inputBuffer.clear();
        inputBuffer.put(packet.payload);
        int flags = !hasCompleteCsd && packet.isCodecConfig()
            ? MediaCodec.BUFFER_FLAG_CODEC_CONFIG
            : 0;
        decoder.queueInputBuffer(inputIndex, 0, packet.payload.length, packet.ptsUs, flags);
    }

    private void notifyPrepared(int width, int height) {
        Activity activity = mActivityReference.get();
        if (activity != null) {
            activity.runOnUiThread(() -> MakepadNative.onVideoPlaybackPrepared(
                mVideoId,
                width,
                height,
                0L,
                BrokerH264VideoPlayer.this));
        } else {
            MakepadNative.onVideoPlaybackPrepared(mVideoId, width, height, 0L, this);
        }
    }

    private void notifyCompleted() {
        Activity activity = mActivityReference.get();
        if (activity != null) {
            activity.runOnUiThread(() -> MakepadNative.onVideoPlaybackCompleted(mVideoId));
        } else {
            MakepadNative.onVideoPlaybackCompleted(mVideoId);
        }
    }

    private static boolean requestDecoderLowLatency(MediaCodec decoder) {
        try {
            MediaCodecInfo.CodecCapabilities capabilities =
                decoder.getCodecInfo().getCapabilitiesForType("video/avc");
            if (!capabilities.isFeatureSupported(MediaCodecInfo.CodecCapabilities.FEATURE_LowLatency)) {
                return false;
            }
            Bundle params = new Bundle();
            params.putInt(MediaCodec.PARAMETER_KEY_LOW_LATENCY, 1);
            decoder.setParameters(params);
            return true;
        } catch (Exception ignored) {
            return false;
        }
    }

    private Socket connectWithRetry(String host, int port, int timeoutMs) throws Exception {
        long deadline = SystemClock.elapsedRealtimeNanos() + (long) timeoutMs * 1_000_000L;
        Exception lastError = null;
        while (mRunning && SystemClock.elapsedRealtimeNanos() < deadline) {
            Socket socket = new Socket();
            try {
                socket.connect(new InetSocketAddress(host, port), 500);
                socket.setTcpNoDelay(true);
                return socket;
            } catch (Exception ex) {
                lastError = ex;
                closeQuietly(socket);
                Thread.sleep(50);
            }
        }
        throw new IllegalStateException(
            "Timed out connecting to broker H.264 stream on port " + port + ": " +
                (lastError != null ? safeMessage(lastError) : ""));
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

    private static NalUnit findNalUnit(List<Packet> packets, int nalType) {
        for (int i = 0; i < packets.size(); i++) {
            byte[] payload = packets.get(i).payload;
            int start = findStartCode(payload, 0);
            while (start >= 0) {
                int startCodeLength = startCodeLengthAt(payload, start);
                int nalStart = start + startCodeLength;
                if (nalStart >= payload.length) {
                    break;
                }
                int nextStart = findStartCode(payload, nalStart);
                int nalEnd = nextStart >= 0 ? nextStart : payload.length;
                if ((payload[nalStart] & 0x1f) == nalType) {
                    byte[] bytes = new byte[nalEnd - start];
                    System.arraycopy(payload, start, bytes, 0, bytes.length);
                    return new NalUnit(bytes);
                }
                start = nextStart;
            }
        }
        return null;
    }

    private static int findStartCode(byte[] data, int offset) {
        for (int i = Math.max(0, offset); i < data.length - 2; i++) {
            if (startCodeLengthAt(data, i) > 0) {
                return i;
            }
        }
        return -1;
    }

    private static int startCodeLengthAt(byte[] data, int offset) {
        if (offset + 4 <= data.length &&
            data[offset] == 0 &&
            data[offset + 1] == 0 &&
            data[offset + 2] == 0 &&
            data[offset + 3] == 1) {
            return 4;
        }
        if (offset + 3 <= data.length &&
            data[offset] == 0 &&
            data[offset + 1] == 0 &&
            data[offset + 2] == 1) {
            return 3;
        }
        return 0;
    }

    private static String normalizeSourceMode(String value) {
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

    private static String normalizeSyntheticPattern(String value) {
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

    private static void closeQuietly(Socket socket) {
        if (socket != null) {
            try {
                socket.close();
            } catch (Exception ignored) {
            }
        }
    }

    private static String safeMessage(Throwable ex) {
        String message = ex.getMessage();
        return message != null ? message : ex.toString();
    }

    static final class Config {
        final String brokerHost;
        final int brokerPort;
        final int streamPort;
        final String sourceMode;
        final String syntheticPattern;
        final int preferredWidth;
        final int preferredHeight;
        final int captureMs;
        final int maxPackets;
        final int bitrateBps;
        final int commandTimeoutMs;
        final int streamTimeoutMs;
        final int decodeTimeoutMs;
        final boolean liveStream;

        Config(
            String brokerHost,
            int brokerPort,
            int streamPort,
            String sourceMode,
            String syntheticPattern,
            int preferredWidth,
            int preferredHeight,
            int captureMs,
            int maxPackets,
            int bitrateBps,
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
            this.syntheticPattern = normalizeSyntheticPattern(syntheticPattern);
            this.preferredWidth = clamp(preferredWidth, 16, 4096);
            this.preferredHeight = clamp(preferredHeight, 16, 4096);
            this.captureMs = clamp(captureMs, 100, 120000);
            this.maxPackets = clamp(maxPackets, 1, MAX_STREAM_PACKETS);
            this.bitrateBps = clamp(bitrateBps, 100000, 20000000);
            this.commandTimeoutMs = clamp(commandTimeoutMs, 500, 60000);
            this.streamTimeoutMs = clamp(streamTimeoutMs, 500, 120000);
            this.decodeTimeoutMs = clamp(decodeTimeoutMs, 500, 60000);
            this.liveStream = liveStream;
        }

        static Config defaults() {
            return new Config(
                "127.0.0.1",
                8765,
                8879,
                "broker-synthetic",
                "diagnostic-grid",
                1280,
                1280,
                900,
                32,
                2000000,
                10000,
                20000,
                5000,
                false);
        }

        private static int clamp(int value, int min, int max) {
            return Math.max(min, Math.min(max, value));
        }
    }

    private static final class StreamHeader {
        final int schemaVersion;
        final int codecId;
        final int width;
        final int height;
        final int packetCount;

        StreamHeader(int schemaVersion, int codecId, int width, int height, int packetCount) {
            this.schemaVersion = schemaVersion;
            this.codecId = codecId;
            this.width = Math.max(1, width);
            this.height = Math.max(1, height);
            this.packetCount = packetCount;
        }
    }

    private static final class Packet {
        final long ptsUs;
        final int flags;
        final byte[] payload;

        Packet(long ptsUs, int flags, byte[] payload) {
            this.ptsUs = ptsUs;
            this.flags = flags;
            this.payload = payload;
        }

        boolean isCodecConfig() {
            return (flags & MediaCodec.BUFFER_FLAG_CODEC_CONFIG) != 0;
        }
    }

    private static final class NalUnit {
        final byte[] bytes;

        NalUnit(byte[] bytes) {
            this.bytes = bytes;
        }
    }
}
