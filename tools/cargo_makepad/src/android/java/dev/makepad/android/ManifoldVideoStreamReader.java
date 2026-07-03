package dev.makepad.android;

import android.media.MediaCodec;
import android.util.Log;

import org.json.JSONObject;

import java.io.DataInputStream;
import java.nio.charset.StandardCharsets;
import java.util.Locale;

final class ManifoldVideoStreamReader {
    static final int CODEC_H264 = 1;

    private static final String STREAM_MAGIC = "RMANVID1";
    private static final String LEGACY_STREAM_MAGIC = "RXYRVID1";
    private static final int MAX_PACKET_BYTES = 1024 * 1024;
    private static final int MAX_STREAM_HEADER_METADATA_BYTES = 256 * 1024;
    private static final int MAX_STREAM_PACKETS = ExternalH264Config.MAX_STREAM_PACKETS;

    private final String logTag;
    private final long videoId;

    ManifoldVideoStreamReader(String logTag, long videoId) {
        this.logTag = logTag;
        this.videoId = videoId;
    }

    StreamHeader readHeader(DataInputStream input) throws Exception {
        byte[] magicBytes = new byte[8];
        input.readFully(magicBytes);
        String magic = new String(magicBytes, StandardCharsets.US_ASCII);
        if (!STREAM_MAGIC.equals(magic) && !LEGACY_STREAM_MAGIC.equals(magic)) {
            throw new IllegalStateException("Unexpected external stream magic: " + magic);
        }

        int schemaVersion = input.readInt();
        int codecId = input.readInt();
        int width = input.readInt();
        int height = input.readInt();
        int packetCount = input.readInt();
        int headerMetadataBytes = input.readInt();
        if (schemaVersion < 1 || schemaVersion > 3) {
            throw new IllegalStateException("Unsupported external stream schema version: " + schemaVersion);
        }
        if (packetCount < 0 || packetCount > MAX_STREAM_PACKETS) {
            throw new IllegalStateException("External stream packet count is out of range: " + packetCount);
        }
        if (headerMetadataBytes < 0 || headerMetadataBytes > MAX_STREAM_HEADER_METADATA_BYTES) {
            throw new IllegalStateException("External stream metadata header is out of range: " + headerMetadataBytes);
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
                Log.w(logTag, String.format(
                    Locale.US,
                    "External H.264 stream header metadata parse failed videoId=%d bytes=%d error=%s",
                    videoId,
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
        Log.i(logTag, String.format(
            Locale.US,
            "External H.264 stream header videoId=%d magic=%s schema=%d width=%d height=%d packets=%d metadataBytes=%d metadataReady=%s cameraId=%s source=%s",
            videoId,
            magic,
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
            STREAM_MAGIC.equals(magic) || schemaVersion >= 2,
            codecId,
            width,
            height,
            packetCount,
            headerMetadataBytes,
            projectionMetadataJson,
            projectionMetadata);
    }

    Packet readPacket(DataInputStream input, StreamHeader header) throws Exception {
        long ptsUs = input.readLong();
        int flags = input.readInt();
        int size = input.readInt();
        if (size < 0 || size > MAX_PACKET_BYTES) {
            throw new IllegalStateException("Broker stream packet size is out of range: " + size);
        }
        long sourceElapsedNs = 0L;
        long sourceUnixNs = 0L;
        if (header.extendedPacketTimestamps) {
            sourceElapsedNs = input.readLong();
            sourceUnixNs = input.readLong();
        }
        byte[] payload = new byte[size];
        input.readFully(payload);
        return new Packet(ptsUs, flags, sourceElapsedNs, sourceUnixNs, payload);
    }

    private static String safeMessage(Throwable ex) {
        String message = ex.getMessage();
        return message != null ? message : ex.getClass().getSimpleName();
    }
}

final class StreamHeader {
    final int schemaVersion;
    final boolean extendedPacketTimestamps;
    final int codecId;
    final int width;
    final int height;
    final int packetCount;
    final int headerMetadataBytes;
    final String projectionMetadataJson;
    final JSONObject projectionMetadata;

    StreamHeader(
        int schemaVersion,
        boolean extendedPacketTimestamps,
        int codecId,
        int width,
        int height,
        int packetCount,
        int headerMetadataBytes,
        String projectionMetadataJson,
        JSONObject projectionMetadata) {
        this.schemaVersion = schemaVersion;
        this.extendedPacketTimestamps = extendedPacketTimestamps;
        this.codecId = codecId;
        this.width = Math.max(1, width);
        this.height = Math.max(1, height);
        this.packetCount = packetCount;
        this.headerMetadataBytes = headerMetadataBytes;
        this.projectionMetadataJson = projectionMetadataJson;
        this.projectionMetadata = projectionMetadata;
    }
}

final class Packet {
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
