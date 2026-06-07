package dev.makepad.android;

import android.app.Activity;

final class ExternalH264VideoPlaybackFactory {
    private ExternalH264VideoPlaybackFactory() {
    }

    static VideoPlayerRunnable createRunnable(
            Activity activity,
            long videoId,
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
            int externalTextureHandle,
            boolean autoplay,
            boolean shouldLoop,
            boolean liveStream) {
        BrokerH264VideoPlayer.Config config = new BrokerH264VideoPlayer.Config(
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
        VideoPlayer videoPlayer = new BrokerH264VideoPlayer(activity, videoId, config);
        videoPlayer.setExternalTextureHandle(externalTextureHandle);
        videoPlayer.setAutoplay(autoplay);
        videoPlayer.setShouldLoop(shouldLoop);
        return new VideoPlayerRunnable(videoPlayer);
    }
}
