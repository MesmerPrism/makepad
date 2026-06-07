package dev.makepad.android;

import android.media.Image;

import java.nio.ByteBuffer;

final class ExternalH264CpuYuvEmitter {
    private ExternalH264CpuYuvEmitter() {
    }

    static void emitFrame(long videoId, Image image, long ptsUs) {
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
        MakepadNative.onVideoYuvFrame(videoId, width, height, ptsUs / 1000L, y, u, v);
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
}
