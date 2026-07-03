package dev.makepad.android;

import android.app.Activity;
import android.graphics.SurfaceTexture;
import android.media.Image;
import android.media.MediaCodec;
import android.media.MediaCodecInfo;
import android.media.MediaFormat;
import android.os.Bundle;
import android.os.SystemClock;
import android.util.Log;
import android.view.Surface;

import org.json.JSONObject;

import java.io.DataInputStream;
import java.io.EOFException;
import java.net.InetSocketAddress;
import java.net.Socket;
import java.nio.ByteBuffer;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.concurrent.atomic.AtomicBoolean;

final class BrokerH264VideoPlayer extends VideoPlayer {
    private static final String TAG = "MakepadExternalH264";
    private static final int DEQUEUE_TIMEOUT_US = 10000;
    private static final int HARDWARE_BUFFER_WAIT_MS = 250;
    private static final long PROGRESS_LOG_INTERVAL_MS = 2000L;
    private static final String DECODE_OUTPUT_AUTO = ExternalH264Config.DECODE_OUTPUT_AUTO;
    private static final String DECODE_OUTPUT_CPU_YUV = ExternalH264Config.DECODE_OUTPUT_CPU_YUV;
    private static final String DECODE_OUTPUT_SURFACE_TEXTURE =
        ExternalH264Config.DECODE_OUTPUT_SURFACE_TEXTURE;
    private static final String DECODE_OUTPUT_HARDWARE_BUFFER =
        ExternalH264Config.DECODE_OUTPUT_HARDWARE_BUFFER;

    private final Config mConfig;
    private final ManifoldH264CommandClient mCommandClient;
    private final ManifoldVideoStreamReader mStreamReader;
    private final AtomicBoolean mStarted = new AtomicBoolean(false);
    private volatile boolean mRunning = true;
    private volatile Socket mBrokerSocket;
    private volatile Socket mStreamSocket;
    private volatile MediaCodec mDecoder;
    private Surface mDecodeSurface;
    private ExternalH264HardwareBufferTarget mHardwareBufferTarget;
    private Thread mDecodeThread;

    BrokerH264VideoPlayer(Activity activity, long videoId, Config config) {
        super(activity, videoId);
        mConfig = config != null ? config : Config.defaults();
        mCommandClient = new ManifoldH264CommandClient(new ManifoldH264CommandClient.Owner() {
            @Override
            public boolean isRunning() {
                return mRunning;
            }

            @Override
            public void setCommandSocket(Socket socket) {
                mBrokerSocket = socket;
            }

            @Override
            public void clearCommandSocket(Socket socket) {
                if (mBrokerSocket == socket) {
                    mBrokerSocket = null;
                }
            }
        });
        mStreamReader = new ManifoldVideoStreamReader(TAG, mVideoId);
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
                "External H.264 prepare videoId=%d sourceMode=%s streamPort=%d cameraId=%s liveStream=%s autoplay=%s externalTexture=%s decodeOutputMode=%s effectiveDecodeOutputMode=%s preferredWidth=%d preferredHeight=%d projectionGeometryProfile=%s sourceSamplingMode=%s targetScreenUvRect=%s syntheticProjectionProfile=%s",
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

                mHandlerThread = new android.os.HandlerThread("ExternalH264SurfaceTexture");
                mHandlerThread.start();
                mGlHandler = new android.os.Handler(mHandlerThread.getLooper());
                mSurfaceTexture.setOnFrameAvailableListener(surfaceTexture -> {
                    mAvailableFrames.incrementAndGet();
                }, mGlHandler);

                mDecodeSurface = new Surface(mSurfaceTexture);
            } else if (usesHardwareBufferOutput()) {
                mHardwareBufferTarget = ExternalH264HardwareBufferTarget.create(
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
            "External H.264 begin videoId=%d sourceMode=%s streamPort=%d liveStream=%s externalTexture=%s decodeOutputMode=%s effectiveDecodeOutputMode=%s",
            mVideoId,
            normalizeSourceMode(mConfig.sourceMode),
            mConfig.streamPort,
            mConfig.liveStream,
            hasExternalTextureHandle(),
            mConfig.decodeOutputMode,
            effectiveDecodeOutputMode()));
        mRunning = true;
        mDecodeThread = new Thread(this::runDecode, "MakepadExternalH264Decode");
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
                JSONObject ack = mCommandClient.sendStartCommand(mConfig, mVideoId);
                if (!ack.optBoolean("accepted", false)) {
                    throw new IllegalStateException(
                        "Broker rejected external H.264 stream command: " + ack.optString("message", ""));
                }
            }
            decodeStream();
            notifyCompleted();
        } catch (Exception ex) {
            if (mRunning) {
                Log.w(TAG, "External H.264 playback failed: " + safeMessage(ex), ex);
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

    private void decodeStream() throws Exception {
        Socket socket = connectWithRetry(mConfig.brokerHost, mConfig.streamPort, mConfig.streamTimeoutMs);
        mStreamSocket = socket;
        socket.setSoTimeout(mConfig.streamTimeoutMs);
        DataInputStream input = new DataInputStream(socket.getInputStream());
        StreamHeader header = mStreamReader.readHeader(input);
        if (header.codecId != ManifoldVideoStreamReader.CODEC_H264) {
            throw new IllegalStateException("External stream codec is not H.264: " + header.codecId);
        }

        List<Packet> pending = new ArrayList<Packet>();
        int packetsRead = 0;
        while (mRunning &&
            shouldReadMorePrimerPackets(header, pending, packetsRead)) {
            Packet packet = mStreamReader.readPacket(input, header);
            pending.add(packet);
            packetsRead++;
        }

        NalUnit sps = H264AnnexBPrimer.findNalUnit(pending, 7);
        NalUnit pps = H264AnnexBPrimer.findNalUnit(pending, 8);
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
            "External H.264 decoder started videoId=%d decoder=%s lowLatencyRequested=%s",
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
                            packet = mStreamReader.readPacket(input, header);
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
                            ExternalH264CpuYuvEmitter.emitFrame(mVideoId, image, info.presentationTimeUs);
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
            throw new IllegalStateException("External H.264 decoder produced no output frames.");
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
            "External H.264 playback progress videoId=%d phase=%s status=ok sourceMode=%s streamPort=%d cameraId=%s decodeOutputMode=%s effectiveDecodeOutputMode=%s preferredWidth=%d preferredHeight=%d requestedFrameRateHz=%d packetsRead=%d inputQueuedCount=%d decodedFrameCount=%d yuvFrameEmitCount=%d hardwareBufferFrameEmitCount=%d yuvCopyTimeMs=%d yuvCopyAvgMs=%.2f outputFormatChangedCount=%d inputEosQueued=%s outputEosSeen=%s elapsedMs=%d packetReadRateHz=%.2f inputQueueRateHz=%.2f decodedFrameRateHz=%.2f yuvFrameEmitRateHz=%.2f hardwareBufferFrameEmitRateHz=%.2f",
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

    private boolean shouldReadMorePrimerPackets(StreamHeader header, List<Packet> pending, int packetsRead) {
        if (header.packetCount > 0 && packetsRead >= header.packetCount) {
            return false;
        }
        if (pending.size() >= 8) {
            return false;
        }
        return H264AnnexBPrimer.findNalUnit(pending, 7) == null ||
            H264AnnexBPrimer.findNalUnit(pending, 8) == null;
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
            "Timed out connecting to external H.264 stream on port " + port + ": " +
                (lastError != null ? safeMessage(lastError) : ""));
    }

    private static String normalizeSourceMode(String value) {
        return ExternalH264Config.normalizeSourceMode(value);
    }

    private static String projectionGeometryProfileForSource(String sourceMode, String value) {
        return ExternalH264Config.projectionGeometryProfileForSource(sourceMode, value);
    }

    private static String normalizeSourceSamplingMode(String value) {
        return ExternalH264Config.normalizeSourceSamplingMode(value);
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

    static final class Config extends ExternalH264Config {
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
            super(
                brokerHost,
                brokerPort,
                streamPort,
                sourceMode,
                decodeOutputMode,
                syntheticPattern,
                syntheticProjectionProfile,
                sourceSamplingMode,
                targetScreenUvRect,
                cameraId,
                stereoPairId,
                stereoPairRole,
                stereoPairMaxDeltaNs,
                preferredWidth,
                preferredHeight,
                captureMs,
                maxPackets,
                bitrateBps,
                frameRateHz,
                commandTimeoutMs,
                streamTimeoutMs,
                decodeTimeoutMs,
                liveStream);
        }

        private Config(ExternalH264Config source) {
            super(source);
        }

        static Config defaults() {
            return new Config(ExternalH264Config.defaults());
        }
    }

}
