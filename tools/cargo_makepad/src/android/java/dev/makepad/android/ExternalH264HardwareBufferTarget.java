package dev.makepad.android;

import android.graphics.ImageFormat;
import android.hardware.HardwareBuffer;
import android.media.Image;
import android.media.ImageReader;
import android.os.SystemClock;
import android.util.Log;
import android.view.Surface;

import java.util.ArrayDeque;
import java.util.HashMap;
import java.util.Locale;
import java.util.Map;

final class ExternalH264HardwareBufferTarget {
    private static final String TAG = "MakepadExternalH264";
    private static final int HARDWARE_BUFFER_READER_MAX_IMAGES = 4;
    private static final int STEREO_HARDWARE_BUFFER_QUEUE_LIMIT = 4;
    private static final long STEREO_HARDWARE_BUFFER_STALE_NS = 250_000_000L;
    private static final Object STEREO_HARDWARE_BUFFER_PAIRER_LOCK = new Object();
    private static final Map<String, StereoHardwareBufferPairer> STEREO_HARDWARE_BUFFER_PAIRERS =
        new HashMap<String, StereoHardwareBufferPairer>();

    private final ImageReader reader;
    private final int width;
    private final int height;

    private ExternalH264HardwareBufferTarget(ImageReader reader, int width, int height) {
        this.reader = reader;
        this.width = width;
        this.height = height;
    }

    static ExternalH264HardwareBufferTarget create(int width, int height) {
        int safeWidth = Math.max(1, width);
        int safeHeight = Math.max(1, height);
        ImageReader reader = ImageReader.newInstance(
            safeWidth,
            safeHeight,
            ImageFormat.PRIVATE,
            HARDWARE_BUFFER_READER_MAX_IMAGES);
        return new ExternalH264HardwareBufferTarget(reader, safeWidth, safeHeight);
    }

    Surface surface() {
        return reader.getSurface();
    }

    boolean awaitAndEmitFrame(
        long videoId,
        ExternalH264Config config,
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
            Log.w(TAG, "Could not emit external H.264 hardware-buffer frame: " + safeMessage(error), error);
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

    private static StereoHardwareBufferPairer stereoHardwareBufferPairer(ExternalH264Config config) {
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

    private static void clearStereoHardwareBufferPairerIfUnused(ExternalH264Config config) {
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

    private static String safeMessage(Throwable ex) {
        String message = ex.getMessage();
        return message != null ? message : ex.toString();
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
                        "External H.264 stereo hardware-buffer pair delivered pairId=%s pairIndex=%d deltaNs=%d leftVideoId=%d rightVideoId=%d leftSeq=%d rightSeq=%d dropped=%d",
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
                Log.w(TAG, "Could not emit external H.264 stereo hardware-buffer pair: " + safeMessage(error), error);
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
                    "External H.264 stereo hardware-buffer frame dropped pairId=%s reason=%s role=%s seq=%d ts=%d dropped=%d",
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
}
