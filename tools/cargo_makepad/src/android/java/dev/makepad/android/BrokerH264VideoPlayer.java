package dev.makepad.android;

import android.app.Activity;
import android.graphics.SurfaceTexture;
import android.graphics.ImageFormat;
import android.hardware.HardwareBuffer;
import android.media.Image;
import android.media.ImageReader;
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
import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;
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
    private static final int HARDWARE_BUFFER_READER_MAX_IMAGES = 4;
    private static final int HARDWARE_BUFFER_WAIT_MS = 250;
    private static final int STEREO_HARDWARE_BUFFER_QUEUE_LIMIT = 4;
    private static final long STEREO_HARDWARE_BUFFER_STALE_NS = 250_000_000L;
    private static final long PROGRESS_LOG_INTERVAL_MS = 2000L;
    private static final String DEFAULT_CAMERA_PROJECTION_GEOMETRY_PROFILE = "full-frame-diagnostic";
    private static final String CAMERA_PROJECTION_GEOMETRY_PROFILE = "camera-projection";
    private static final String DECODE_OUTPUT_AUTO = "auto";
    private static final String DECODE_OUTPUT_CPU_YUV = "cpu-yuv";
    private static final String DECODE_OUTPUT_SURFACE_TEXTURE = "surface-texture";
    private static final String DECODE_OUTPUT_HARDWARE_BUFFER = "hardware-buffer";
    private static final String SOURCE_SAMPLING_TARGET_LOCAL_RASTER = "target-local-raster";
    private static final String SOURCE_SAMPLING_SCREEN_TO_CAMERA_HOMOGRAPHY = "screen-to-camera-homography";
    private static final Object STEREO_HARDWARE_BUFFER_PAIRER_LOCK = new Object();
    private static final Map<String, StereoHardwareBufferPairer> STEREO_HARDWARE_BUFFER_PAIRERS =
        new HashMap<String, StereoHardwareBufferPairer>();

    private final Config mConfig;
    private final AtomicBoolean mStarted = new AtomicBoolean(false);
    private volatile boolean mRunning = true;
    private volatile Socket mBrokerSocket;
    private volatile Socket mStreamSocket;
    private volatile MediaCodec mDecoder;
    private Surface mDecodeSurface;
    private DecodeHardwareBufferTarget mHardwareBufferTarget;
    private Thread mDecodeThread;

    BrokerH264VideoPlayer(Activity activity, long videoId, Config config) {
        super(activity, videoId);
        mConfig = config != null ? config : Config.defaults();
    }

    @Override
    public void prepareVideoPlayback() {
        try {
            String sourceMode = normalizeSourceMode(mConfig.sourceMode);
            String projectionGeometryProfile =
                projectionGeometryProfileForSource(sourceMode, mConfig.syntheticProjectionProfile);
            String sourceSamplingMode = normalizeSourceSamplingMode(mConfig.sourceSamplingMode);
            Log.i(TAG, String.format(
                Locale.US,
                "Broker H.264 prepare videoId=%d sourceMode=%s streamPort=%d cameraId=%s liveStream=%s autoplay=%s externalTexture=%s decodeOutputMode=%s effectiveDecodeOutputMode=%s preferredWidth=%d preferredHeight=%d projectionGeometryProfile=%s sourceSamplingMode=%s targetScreenUvRect=%s syntheticProjectionProfile=%s",
                mVideoId,
                sourceMode,
                mConfig.streamPort,
                mConfig.cameraId,
                mConfig.liveStream,
                mAutoplay,
                hasExternalTextureHandle(),
                mConfig.decodeOutputMode,
                effectiveDecodeOutputMode(),
                mConfig.preferredWidth,
                mConfig.preferredHeight,
                projectionGeometryProfile,
                sourceSamplingMode,
                mConfig.targetScreenUvRect,
                mConfig.syntheticProjectionProfile));
            if (usesSurfaceTextureOutput()) {
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
            } else if (usesHardwareBufferOutput()) {
                mHardwareBufferTarget = DecodeHardwareBufferTarget.create(
                    mConfig.preferredWidth,
                    mConfig.preferredHeight);
                mDecodeSurface = mHardwareBufferTarget.surface();
            }
            mIsPrepared = true;
            if (mAutoplay || mConfig.liveStream) {
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
        Log.i(TAG, String.format(
            Locale.US,
            "Broker H.264 begin videoId=%d sourceMode=%s streamPort=%d liveStream=%s externalTexture=%s decodeOutputMode=%s effectiveDecodeOutputMode=%s",
            mVideoId,
            normalizeSourceMode(mConfig.sourceMode),
            mConfig.streamPort,
            mConfig.liveStream,
            hasExternalTextureHandle(),
            mConfig.decodeOutputMode,
            effectiveDecodeOutputMode()));
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
        if (mHardwareBufferTarget != null) {
            mHardwareBufferTarget.close();
            mHardwareBufferTarget = null;
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

    private boolean hasExternalTextureHandle() {
        return mExternalTextureHandle > 0;
    }

    private boolean usesSurfaceTextureOutput() {
        String outputMode = effectiveDecodeOutputMode();
        return DECODE_OUTPUT_SURFACE_TEXTURE.equals(outputMode);
    }

    private boolean usesHardwareBufferOutput() {
        String outputMode = effectiveDecodeOutputMode();
        return DECODE_OUTPUT_HARDWARE_BUFFER.equals(outputMode);
    }

    private boolean usesCpuYuvOutput() {
        String outputMode = effectiveDecodeOutputMode();
        return DECODE_OUTPUT_CPU_YUV.equals(outputMode);
    }

    private String effectiveDecodeOutputMode() {
        if (DECODE_OUTPUT_AUTO.equals(mConfig.decodeOutputMode)) {
            return hasExternalTextureHandle()
                ? DECODE_OUTPUT_SURFACE_TEXTURE
                : DECODE_OUTPUT_CPU_YUV;
        }
        return mConfig.decodeOutputMode;
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
        String projectionGeometryProfile =
            projectionGeometryProfileForSource(sourceMode, mConfig.syntheticProjectionProfile);
        JSONObject params = new JSONObject();
        params.put("device_port", mConfig.streamPort);
        params.put("host_port", mConfig.streamPort);
        params.put("preferred_width", mConfig.preferredWidth);
        params.put("preferred_height", mConfig.preferredHeight);
        params.put("content_width", mConfig.preferredWidth);
        params.put("content_height", mConfig.preferredHeight);
        params.put(
            "desired_display_aspect_ratio",
            mConfig.preferredHeight > 0
                ? (double) mConfig.preferredWidth / (double) mConfig.preferredHeight
                : 1.0);
        params.put("capture_ms", mConfig.captureMs);
        params.put("max_packets", mConfig.maxPackets);
        params.put("bitrate_bps", mConfig.bitrateBps);
        params.put("frame_rate_hz", mConfig.frameRateHz);
        params.put("live_stream", mConfig.liveStream);
        params.put("projection_geometry_profile", projectionGeometryProfile);
        params.put("projectionGeometryProfile", projectionGeometryProfile);
        String sourceSamplingMode = normalizeSourceSamplingMode(mConfig.sourceSamplingMode);
        if (sourceSamplingMode.length() > 0) {
            params.put("source_sampling_mode", sourceSamplingMode);
            params.put("sourceSamplingMode", sourceSamplingMode);
        }
        if (mConfig.targetScreenUvRect.length() > 0) {
            params.put("target_screen_uv_rect", mConfig.targetScreenUvRect);
            params.put("targetScreenUvRect", mConfig.targetScreenUvRect);
        }
        if (mConfig.stereoPairId.length() > 0 && mConfig.stereoPairRole.length() > 0) {
            params.put("stereo_pair_release", true);
            params.put("stereo_pair_id", mConfig.stereoPairId);
            params.put("stereoPairId", mConfig.stereoPairId);
            params.put("stereo_pair_role", mConfig.stereoPairRole);
            params.put("stereoPairRole", mConfig.stereoPairRole);
            params.put("stereo_pair_max_delta_ns", mConfig.stereoPairMaxDeltaNs);
            params.put("stereoPairMaxDeltaNs", mConfig.stereoPairMaxDeltaNs);
        }
        if ("broker-synthetic".equals(sourceMode)) {
            params.put("source_mode", "synthetic_surface");
            params.put("synthetic_pattern", normalizeSyntheticPattern(mConfig.syntheticPattern));
            params.put(
                "synthetic_projection_profile",
                normalizeSyntheticProjectionProfile(mConfig.syntheticProjectionProfile));
        }
        params.put("camera_id", mConfig.cameraId);

        JSONObject command = new JSONObject();
        command.put("type", "command");
        command.put("schema", "rusty.xr.broker.command.v1");
        String clientLabel = mConfig.stereoPairRole.length() > 0
            ? mConfig.stereoPairRole
            : Long.toString(mVideoId);
        command.put(
            "request_id",
            "makepad-h264-video-" + clientLabel + "-" + mVideoId + "-" + System.currentTimeMillis());
        command.put(
            "command",
            "broker-synthetic".equals(sourceMode)
                ? "media.start_synthetic_h264_stream"
                : "camera_provider.start_app_camera_h264_stream");
        command.put("client_id", "makepad-broker-h264-video-" + clientLabel);
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
        if (usesCpuYuvOutput()) {
            try {
                format.setInteger(
                    MediaFormat.KEY_COLOR_FORMAT,
                    MediaCodecInfo.CodecCapabilities.COLOR_FormatYUV420Flexible);
            } catch (Exception ignored) {
            }
        }

        MediaCodec decoder = MediaCodec.createDecoderByType("video/avc");
        mDecoder = decoder;
        decoder.configure(format, mDecodeSurface, null, 0);
        decoder.start();
        boolean lowLatencyParameterSucceeded = requestDecoderLowLatency(decoder);
        Log.i(TAG, String.format(
            Locale.US,
            "Broker H.264 decoder started videoId=%d decoder=%s lowLatencyRequested=%s",
            mVideoId,
            decoder.getName(),
            lowLatencyParameterSucceeded));
        notifyPrepared(header);

        MediaCodec.BufferInfo info = new MediaCodec.BufferInfo();
        int nextPending = 0;
        long lastPtsUs = 0L;
        boolean inputEosQueued = false;
        boolean outputEosSeen = false;
        int inputQueuedCount = 0;
        int decodedFrameCount = 0;
        int outputFormatChangedCount = 0;
        int yuvFrameEmitCount = 0;
        int hardwareBufferFrameEmitCount = 0;
        long yuvCopyTimeMs = 0L;
        Map<Long, Long> sourceElapsedByPts = new HashMap<Long, Long>();
        long progressStartMs = SystemClock.elapsedRealtime();
        long lastProgressMs = progressStartMs;
        long deadline = mConfig.isUnboundedLiveStream()
            ? Long.MAX_VALUE
            : SystemClock.elapsedRealtimeNanos() +
                (long) Math.max(
                    1,
                    mConfig.decodeTimeoutMs + mConfig.streamTimeoutMs + mConfig.captureMs) * 1_000_000L;

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
                        recordQueuedPacketTiming(sourceElapsedByPts, packet);
                        queuePacket(decoder, inputIndex, packet, hasCompleteCsd);
                        inputQueuedCount++;
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
                lastProgressMs = maybeLogProgress(
                    "progress",
                    progressStartMs,
                    lastProgressMs,
                    packetsRead,
                    inputQueuedCount,
                    decodedFrameCount,
                    yuvFrameEmitCount,
                    hardwareBufferFrameEmitCount,
                    yuvCopyTimeMs,
                    outputFormatChangedCount,
                    inputEosQueued,
                    outputEosSeen);
                continue;
            }
            if (outputIndex == MediaCodec.INFO_OUTPUT_FORMAT_CHANGED) {
                outputFormatChangedCount++;
                continue;
            }
            if (outputIndex < 0) {
                continue;
            }

            boolean codecConfig = (info.flags & MediaCodec.BUFFER_FLAG_CODEC_CONFIG) != 0;
            boolean eos = (info.flags & MediaCodec.BUFFER_FLAG_END_OF_STREAM) != 0;
            boolean hardwareBufferOutput = usesHardwareBufferOutput() && !codecConfig && !eos;
            boolean renderToSurface = (usesSurfaceTextureOutput() || hardwareBufferOutput) &&
                !codecConfig &&
                !eos;
            long decodedFrameSequence = decodedFrameCount + 1L;
            if (!codecConfig && !eos) {
                if (usesCpuYuvOutput()) {
                    Image image = decoder.getOutputImage(outputIndex);
                    if (image != null) {
                        try {
                            long copyStartMs = SystemClock.elapsedRealtime();
                            emitYuvFrame(image, info.presentationTimeUs);
                            yuvCopyTimeMs += Math.max(
                                0L,
                                SystemClock.elapsedRealtime() - copyStartMs);
                            yuvFrameEmitCount++;
                        } finally {
                            image.close();
                        }
                    }
                }
                decodedFrameCount++;
            }
            decoder.releaseOutputBuffer(outputIndex, renderToSurface);
            if (hardwareBufferOutput && mHardwareBufferTarget != null) {
                if (mHardwareBufferTarget.awaitAndEmitFrame(
                    mVideoId,
                    mConfig,
                    HARDWARE_BUFFER_WAIT_MS,
                    decodedFrameSequence,
                    info.presentationTimeUs,
                    sourceElapsedNsForPts(sourceElapsedByPts, info.presentationTimeUs))) {
                    hardwareBufferFrameEmitCount++;
                }
            }
            outputEosSeen = eos;
            lastProgressMs = maybeLogProgress(
                "progress",
                progressStartMs,
                lastProgressMs,
                packetsRead,
                inputQueuedCount,
                decodedFrameCount,
                yuvFrameEmitCount,
                hardwareBufferFrameEmitCount,
                yuvCopyTimeMs,
                outputFormatChangedCount,
                inputEosQueued,
                outputEosSeen);
        }

        logProgress(
            "complete",
            progressStartMs,
            packetsRead,
            inputQueuedCount,
            decodedFrameCount,
            yuvFrameEmitCount,
            hardwareBufferFrameEmitCount,
            yuvCopyTimeMs,
            outputFormatChangedCount,
            inputEosQueued,
            outputEosSeen);

        if (decodedFrameCount == 0 && mRunning) {
            throw new IllegalStateException("Broker H.264 decoder produced no output frames.");
        }
    }

    private long maybeLogProgress(
        String phase,
        long progressStartMs,
        long lastProgressMs,
        int packetsRead,
        int inputQueuedCount,
        int decodedFrameCount,
        int yuvFrameEmitCount,
        int hardwareBufferFrameEmitCount,
        long yuvCopyTimeMs,
        int outputFormatChangedCount,
        boolean inputEosQueued,
        boolean outputEosSeen) {
        long nowMs = SystemClock.elapsedRealtime();
        if (nowMs - lastProgressMs < PROGRESS_LOG_INTERVAL_MS) {
            return lastProgressMs;
        }
        logProgress(
            phase,
            progressStartMs,
            packetsRead,
            inputQueuedCount,
            decodedFrameCount,
            yuvFrameEmitCount,
            hardwareBufferFrameEmitCount,
            yuvCopyTimeMs,
            outputFormatChangedCount,
            inputEosQueued,
            outputEosSeen);
        return nowMs;
    }

    private void logProgress(
        String phase,
        long progressStartMs,
        int packetsRead,
        int inputQueuedCount,
        int decodedFrameCount,
        int yuvFrameEmitCount,
        int hardwareBufferFrameEmitCount,
        long yuvCopyTimeMs,
        int outputFormatChangedCount,
        boolean inputEosQueued,
        boolean outputEosSeen) {
        long elapsedMs = Math.max(1L, SystemClock.elapsedRealtime() - progressStartMs);
        double elapsedSeconds = elapsedMs / 1000.0;
        double averageYuvCopyMs = yuvFrameEmitCount > 0
            ? yuvCopyTimeMs / (double) yuvFrameEmitCount
            : 0.0;
        Log.i(TAG, String.format(
            Locale.US,
            "Broker H.264 playback progress videoId=%d phase=%s status=ok sourceMode=%s streamPort=%d cameraId=%s decodeOutputMode=%s effectiveDecodeOutputMode=%s preferredWidth=%d preferredHeight=%d requestedFrameRateHz=%d packetsRead=%d inputQueuedCount=%d decodedFrameCount=%d yuvFrameEmitCount=%d hardwareBufferFrameEmitCount=%d yuvCopyTimeMs=%d yuvCopyAvgMs=%.2f outputFormatChangedCount=%d inputEosQueued=%s outputEosSeen=%s elapsedMs=%d packetReadRateHz=%.2f inputQueueRateHz=%.2f decodedFrameRateHz=%.2f yuvFrameEmitRateHz=%.2f hardwareBufferFrameEmitRateHz=%.2f",
            mVideoId,
            phase,
            normalizeSourceMode(mConfig.sourceMode),
            mConfig.streamPort,
            mConfig.cameraId,
            mConfig.decodeOutputMode,
            effectiveDecodeOutputMode(),
            mConfig.preferredWidth,
            mConfig.preferredHeight,
            mConfig.frameRateHz,
            packetsRead,
            inputQueuedCount,
            decodedFrameCount,
            yuvFrameEmitCount,
            hardwareBufferFrameEmitCount,
            yuvCopyTimeMs,
            averageYuvCopyMs,
            outputFormatChangedCount,
            inputEosQueued,
            outputEosSeen,
            elapsedMs,
            packetsRead / elapsedSeconds,
            inputQueuedCount / elapsedSeconds,
            decodedFrameCount / elapsedSeconds,
            yuvFrameEmitCount / elapsedSeconds,
            hardwareBufferFrameEmitCount / elapsedSeconds));
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
        JSONObject projectionMetadata = null;
        String projectionMetadataJson = "";
        if (headerMetadataBytes > 0) {
            byte[] metadataBytes = new byte[headerMetadataBytes];
            input.readFully(metadataBytes);
            String metadataJson = new String(metadataBytes, StandardCharsets.UTF_8);
            projectionMetadataJson = metadataJson;
            try {
                projectionMetadata = new JSONObject(metadataJson);
            } catch (Exception ex) {
                Log.w(TAG, String.format(
                    Locale.US,
                    "Broker H.264 stream header metadata parse failed videoId=%d bytes=%d error=%s",
                    mVideoId,
                    headerMetadataBytes,
                    safeMessage(ex)));
            }
        }

        boolean metadataReady = projectionMetadata != null &&
            projectionMetadata.optBoolean("projectionMetadataReady", false);
        String metadataCameraId = projectionMetadata != null
            ? projectionMetadata.optString("cameraId", "")
            : "";
        String metadataSource = projectionMetadata != null
            ? projectionMetadata.optString("source", "")
            : "";
        Log.i(TAG, String.format(
            Locale.US,
            "Broker H.264 stream header videoId=%d schema=%d width=%d height=%d packets=%d metadataBytes=%d metadataReady=%s cameraId=%s source=%s",
            mVideoId,
            schemaVersion,
            width,
            height,
            packetCount,
            headerMetadataBytes,
            metadataReady,
            metadataCameraId,
            metadataSource));
        return new StreamHeader(
            schemaVersion,
            codecId,
            width,
            height,
            packetCount,
            headerMetadataBytes,
            projectionMetadataJson,
            projectionMetadata);
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
        long sourceElapsedNs = 0L;
        long sourceUnixNs = 0L;
        if (schemaVersion >= 2) {
            sourceElapsedNs = input.readLong();
            sourceUnixNs = input.readLong();
        }
        byte[] payload = new byte[size];
        input.readFully(payload);
        return new Packet(ptsUs, flags, sourceElapsedNs, sourceUnixNs, payload);
    }

    private static void recordQueuedPacketTiming(Map<Long, Long> sourceElapsedByPts, Packet packet) {
        if (packet.sourceElapsedNs > 0L) {
            sourceElapsedByPts.put(Long.valueOf(packet.ptsUs), Long.valueOf(packet.sourceElapsedNs));
        }
    }

    private static long sourceElapsedNsForPts(Map<Long, Long> sourceElapsedByPts, long ptsUs) {
        Long exact = sourceElapsedByPts.get(Long.valueOf(ptsUs));
        if (exact != null && exact.longValue() > 0L) {
            return exact.longValue();
        }
        long closest = 0L;
        long closestDelta = Long.MAX_VALUE;
        for (Map.Entry<Long, Long> entry : sourceElapsedByPts.entrySet()) {
            long value = entry.getValue().longValue();
            if (value <= 0L) {
                continue;
            }
            long delta = Math.abs(entry.getKey().longValue() - ptsUs);
            if (delta < closestDelta) {
                closestDelta = delta;
                closest = value;
            }
        }
        return closest;
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

    private void emitYuvFrame(Image image, long ptsUs) {
        Image.Plane[] planes = image.getPlanes();
        if (planes == null || planes.length < 3) {
            return;
        }
        int width = Math.max(1, image.getWidth());
        int height = Math.max(1, image.getHeight());
        int chromaWidth = (width + 1) / 2;
        int chromaHeight = (height + 1) / 2;
        byte[] y = copyPlane(planes[0], width, height);
        byte[] u = copyPlane(planes[1], chromaWidth, chromaHeight);
        byte[] v = copyPlane(planes[2], chromaWidth, chromaHeight);
        MakepadNative.onVideoYuvFrame(mVideoId, width, height, ptsUs / 1000L, y, u, v);
    }

    private static byte[] copyPlane(Image.Plane plane, int width, int height) {
        ByteBuffer buffer = plane.getBuffer().duplicate();
        int rowStride = Math.max(1, plane.getRowStride());
        int pixelStride = Math.max(1, plane.getPixelStride());
        int base = buffer.position();
        int limit = buffer.limit();
        byte[] out = new byte[Math.max(0, width * height)];

        if (pixelStride == 1) {
            if (rowStride == width && base + out.length <= limit) {
                buffer.position(base);
                buffer.get(out, 0, out.length);
                return out;
            }

            int dst = 0;
            for (int y = 0; y < height; y++) {
                int row = base + y * rowStride;
                int available = row >= 0 && row < limit
                    ? Math.min(width, limit - row)
                    : 0;
                if (available > 0) {
                    buffer.position(row);
                    buffer.get(out, dst, available);
                }
                dst += width;
            }
            return out;
        }

        int dst = 0;
        for (int y = 0; y < height; y++) {
            int row = base + y * rowStride;
            for (int x = 0; x < width; x++) {
                int src = row + x * pixelStride;
                out[dst++] = src >= 0 && src < limit ? buffer.get(src) : 0;
            }
        }
        return out;
    }

    private void notifyPrepared(StreamHeader header) {
        Activity activity = mActivityReference.get();
        String metadataJson = header.projectionMetadataJson != null
            ? header.projectionMetadataJson
            : "";
        VideoPlayer preparedSurface = usesSurfaceTextureOutput() ? BrokerH264VideoPlayer.this : null;
        if (activity != null) {
            activity.runOnUiThread(() -> {
                if (metadataJson.length() > 0) {
                    MakepadNative.onVideoPlaybackMetadata(mVideoId, metadataJson);
                }
                MakepadNative.onVideoPlaybackPrepared(
                    mVideoId,
                    header.width,
                    header.height,
                    0L,
                    preparedSurface);
            });
        } else {
            if (metadataJson.length() > 0) {
                MakepadNative.onVideoPlaybackMetadata(mVideoId, metadataJson);
            }
            MakepadNative.onVideoPlaybackPrepared(mVideoId, header.width, header.height, 0L, preparedSurface);
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
        Bundle params = new Bundle();
        params.putInt(MediaCodec.PARAMETER_KEY_LOW_LATENCY, 1);
        try {
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

    private static String normalizeDecodeOutputMode(String value) {
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

    private static String normalizeStereoPairRole(String value) {
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

    private static String normalizeSyntheticProjectionProfile(String value) {
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

    private static String normalizeCameraProjectionGeometryProfile(String value) {
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

    private static String projectionGeometryProfileForSource(String sourceMode, String value) {
        if ("broker-camera".equals(normalizeSourceMode(sourceMode))) {
            return normalizeCameraProjectionGeometryProfile(value);
        }
        return normalizeSyntheticProjectionProfile(value);
    }

    private static String normalizeSourceSamplingMode(String value) {
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
        throw new IllegalArgumentException("Unsupported broker H.264 source sampling mode: " + value);
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

    private static StereoHardwareBufferPairer stereoHardwareBufferPairer(Config config) {
        if (config == null || !config.usesStereoHardwareBufferPairing()) {
            return null;
        }
        synchronized (STEREO_HARDWARE_BUFFER_PAIRER_LOCK) {
            StereoHardwareBufferPairer pairer = STEREO_HARDWARE_BUFFER_PAIRERS.get(config.stereoPairId);
            if (pairer == null) {
                pairer = new StereoHardwareBufferPairer(
                    config.stereoPairId,
                    Math.max(1L, config.stereoPairMaxDeltaNs));
                STEREO_HARDWARE_BUFFER_PAIRERS.put(config.stereoPairId, pairer);
            }
            return pairer;
        }
    }

    private static void clearStereoHardwareBufferPairerIfUnused(Config config) {
        if (config == null || config.stereoPairId.length() == 0) {
            return;
        }
        synchronized (STEREO_HARDWARE_BUFFER_PAIRER_LOCK) {
            StereoHardwareBufferPairer pairer = STEREO_HARDWARE_BUFFER_PAIRERS.remove(config.stereoPairId);
            if (pairer != null) {
                pairer.close();
            }
        }
    }

    private static final class HardwareBufferFrame {
        final long videoId;
        final String role;
        final int width;
        final int height;
        final long positionMs;
        final long frameSequence;
        final long timestampNs;
        final long retainedElapsedNs;
        HardwareBuffer buffer;

        HardwareBufferFrame(
            long videoId,
            String role,
            int width,
            int height,
            long positionMs,
            long frameSequence,
            long timestampNs,
            HardwareBuffer buffer) {
            this.videoId = videoId;
            this.role = role;
            this.width = width;
            this.height = height;
            this.positionMs = positionMs;
            this.frameSequence = frameSequence;
            this.timestampNs = timestampNs;
            this.retainedElapsedNs = SystemClock.elapsedRealtimeNanos();
            this.buffer = buffer;
        }

        void close() {
            HardwareBuffer toClose = buffer;
            buffer = null;
            if (toClose != null) {
                try {
                    toClose.close();
                } catch (RuntimeException ignored) {
                }
            }
        }
    }

    private static final class StereoHardwareBufferPairer {
        private final String pairId;
        private final long maxDeltaNs;
        private final ArrayDeque<HardwareBufferFrame> leftFrames =
            new ArrayDeque<HardwareBufferFrame>();
        private final ArrayDeque<HardwareBufferFrame> rightFrames =
            new ArrayDeque<HardwareBufferFrame>();
        private long pairCount;
        private long dropCount;

        StereoHardwareBufferPairer(String pairId, long maxDeltaNs) {
            this.pairId = pairId;
            this.maxDeltaNs = maxDeltaNs;
        }

        synchronized boolean offer(HardwareBufferFrame frame) {
            ArrayDeque<HardwareBufferFrame> queue = "right".equals(frame.role)
                ? rightFrames
                : leftFrames;
            queue.addLast(frame);
            while (queue.size() > STEREO_HARDWARE_BUFFER_QUEUE_LIMIT) {
                dropFrame(queue.removeFirst(), "queue-limit");
            }
            deliverAvailablePairs();
            return true;
        }

        synchronized void close() {
            closeQueue(leftFrames);
            closeQueue(rightFrames);
        }

        private void deliverAvailablePairs() {
            while (true) {
                long nowNs = SystemClock.elapsedRealtimeNanos();
                dropStaleFrames(leftFrames, nowNs);
                dropStaleFrames(rightFrames, nowNs);
                if (leftFrames.isEmpty() || rightFrames.isEmpty()) {
                    return;
                }

                HardwareBufferFrame left = null;
                HardwareBufferFrame right = null;
                long bestDeltaNs = Long.MAX_VALUE;
                for (HardwareBufferFrame leftCandidate : leftFrames) {
                    for (HardwareBufferFrame rightCandidate : rightFrames) {
                        long deltaNs = Math.abs(leftCandidate.timestampNs - rightCandidate.timestampNs);
                        if (deltaNs < bestDeltaNs) {
                            bestDeltaNs = deltaNs;
                            left = leftCandidate;
                            right = rightCandidate;
                        }
                    }
                }
                if (left == null || right == null) {
                    return;
                }
                if (bestDeltaNs > maxDeltaNs) {
                    if (left.timestampNs <= right.timestampNs) {
                        leftFrames.remove(left);
                        dropFrame(left, "skew");
                    } else {
                        rightFrames.remove(right);
                        dropFrame(right, "skew");
                    }
                    continue;
                }

                leftFrames.remove(left);
                rightFrames.remove(right);
                deliverPair(left, right, bestDeltaNs);
            }
        }

        private void deliverPair(HardwareBufferFrame left, HardwareBufferFrame right, long deltaNs) {
            long pairIndex = pairCount++;
            try {
                MakepadNative.onVideoHardwareBufferStereoFrame(
                    left.videoId,
                    left.width,
                    left.height,
                    left.positionMs,
                    left.frameSequence,
                    left.timestampNs,
                    left.buffer,
                    right.videoId,
                    right.width,
                    right.height,
                    right.positionMs,
                    right.frameSequence,
                    right.timestampNs,
                    right.buffer,
                    deltaNs,
                    pairIndex);
                if (pairIndex < 8 || pairIndex % 120 == 0) {
                    Log.i(TAG, String.format(
                        Locale.US,
                        "Broker H.264 stereo hardware-buffer pair delivered pairId=%s pairIndex=%d deltaNs=%d leftVideoId=%d rightVideoId=%d leftSeq=%d rightSeq=%d dropped=%d",
                        pairId,
                        pairIndex,
                        deltaNs,
                        left.videoId,
                        right.videoId,
                        left.frameSequence,
                        right.frameSequence,
                        dropCount));
                }
            } catch (RuntimeException error) {
                Log.w(TAG, "Could not emit broker H.264 stereo hardware-buffer pair: " + safeMessage(error), error);
            } finally {
                left.close();
                right.close();
            }
        }

        private void dropStaleFrames(ArrayDeque<HardwareBufferFrame> queue, long nowNs) {
            while (!queue.isEmpty()) {
                HardwareBufferFrame frame = queue.peekFirst();
                long ageNs = Math.max(0L, nowNs - frame.retainedElapsedNs);
                if (ageNs <= STEREO_HARDWARE_BUFFER_STALE_NS) {
                    return;
                }
                dropFrame(queue.removeFirst(), "stale");
            }
        }

        private void dropFrame(HardwareBufferFrame frame, String reason) {
            dropCount++;
            if (dropCount < 8 || dropCount % 120 == 0) {
                Log.w(TAG, String.format(
                    Locale.US,
                    "Broker H.264 stereo hardware-buffer frame dropped pairId=%s reason=%s role=%s seq=%d ts=%d dropped=%d",
                    pairId,
                    reason,
                    frame.role,
                    frame.frameSequence,
                    frame.timestampNs,
                    dropCount));
            }
            frame.close();
        }

        private static void closeQueue(ArrayDeque<HardwareBufferFrame> queue) {
            while (!queue.isEmpty()) {
                queue.removeFirst().close();
            }
        }
    }

    private static final class DecodeHardwareBufferTarget {
        private final ImageReader reader;
        private final int width;
        private final int height;

        private DecodeHardwareBufferTarget(ImageReader reader, int width, int height) {
            this.reader = reader;
            this.width = width;
            this.height = height;
        }

        static DecodeHardwareBufferTarget create(int width, int height) {
            int safeWidth = Math.max(1, width);
            int safeHeight = Math.max(1, height);
            ImageReader reader = ImageReader.newInstance(
                safeWidth,
                safeHeight,
                ImageFormat.PRIVATE,
                HARDWARE_BUFFER_READER_MAX_IMAGES);
            return new DecodeHardwareBufferTarget(reader, safeWidth, safeHeight);
        }

        Surface surface() {
            return reader.getSurface();
        }

        boolean awaitAndEmitFrame(
            long videoId,
            Config config,
            int timeoutMs,
            long frameSequence,
            long presentationTimeUs,
            long sourceElapsedNs) {
            long deadline = SystemClock.elapsedRealtime() + Math.max(1, timeoutMs);
            Image image = null;
            while (SystemClock.elapsedRealtime() < deadline) {
                try {
                    image = reader.acquireNextImage();
                } catch (IllegalStateException error) {
                    return false;
                }
                if (image != null) {
                    break;
                }
                SystemClock.sleep(2);
            }
            if (image == null) {
                return false;
            }

            HardwareBuffer buffer = null;
            try {
                buffer = image.getHardwareBuffer();
                if (buffer == null) {
                    return false;
                }
                long timestampNs = sourceElapsedNs > 0L ? sourceElapsedNs : image.getTimestamp();
                if (timestampNs <= 0L && presentationTimeUs > 0L) {
                    timestampNs = presentationTimeUs * 1000L;
                }
                if (timestampNs <= 0L) {
                    timestampNs = SystemClock.elapsedRealtimeNanos();
                }
                if (config != null && config.usesStereoHardwareBufferPairing()) {
                    HardwareBufferFrame frame = new HardwareBufferFrame(
                        videoId,
                        config.stereoPairRole,
                        image.getWidth() > 0 ? image.getWidth() : width,
                        image.getHeight() > 0 ? image.getHeight() : height,
                        Math.max(0L, presentationTimeUs / 1000L),
                        Math.max(0L, frameSequence),
                        timestampNs,
                        buffer);
                    buffer = null;
                    StereoHardwareBufferPairer pairer = stereoHardwareBufferPairer(config);
                    return pairer != null && pairer.offer(frame);
                }
                MakepadNative.onVideoHardwareBufferFrame(
                    videoId,
                    image.getWidth() > 0 ? image.getWidth() : width,
                    image.getHeight() > 0 ? image.getHeight() : height,
                    Math.max(0L, presentationTimeUs / 1000L),
                    Math.max(0L, frameSequence),
                    timestampNs,
                    buffer);
                return true;
            } catch (Exception error) {
                Log.w(TAG, "Could not emit broker H.264 hardware-buffer frame: " + safeMessage(error), error);
                return false;
            } finally {
                if (buffer != null) {
                    try {
                        buffer.close();
                    } catch (Exception ignored) {
                    }
                }
                image.close();
            }
        }

        void close() {
            reader.close();
        }
    }

    static final class Config {
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

        Config(
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

        boolean isUnboundedLiveStream() {
            return liveStream && captureMs == 0 && maxPackets == 0;
        }

        boolean usesStereoHardwareBufferPairing() {
            return stereoPairId.length() > 0 &&
                ("left".equals(stereoPairRole) || "right".equals(stereoPairRole));
        }

        static Config defaults() {
            return new Config(
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
        final int headerMetadataBytes;
        final String projectionMetadataJson;
        final JSONObject projectionMetadata;

        StreamHeader(
            int schemaVersion,
            int codecId,
            int width,
            int height,
            int packetCount,
            int headerMetadataBytes,
            String projectionMetadataJson,
            JSONObject projectionMetadata) {
            this.schemaVersion = schemaVersion;
            this.codecId = codecId;
            this.width = Math.max(1, width);
            this.height = Math.max(1, height);
            this.packetCount = packetCount;
            this.headerMetadataBytes = headerMetadataBytes;
            this.projectionMetadataJson = projectionMetadataJson;
            this.projectionMetadata = projectionMetadata;
        }
    }

    private static final class Packet {
        final long ptsUs;
        final int flags;
        final long sourceElapsedNs;
        final long sourceUnixNs;
        final byte[] payload;

        Packet(long ptsUs, int flags, long sourceElapsedNs, long sourceUnixNs, byte[] payload) {
            this.ptsUs = ptsUs;
            this.flags = flags;
            this.sourceElapsedNs = sourceElapsedNs;
            this.sourceUnixNs = sourceUnixNs;
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
