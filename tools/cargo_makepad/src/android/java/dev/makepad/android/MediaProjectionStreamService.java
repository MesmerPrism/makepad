package dev.makepad.android;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.content.pm.ServiceInfo;
import android.graphics.PixelFormat;
import android.hardware.display.DisplayManager;
import android.hardware.display.VirtualDisplay;
import android.media.Image;
import android.media.ImageReader;
import android.media.projection.MediaProjection;
import android.media.projection.MediaProjectionManager;
import android.os.Handler;
import android.os.HandlerThread;
import android.os.IBinder;
import android.util.Log;

import java.io.BufferedOutputStream;
import java.io.IOException;
import java.net.Socket;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.charset.StandardCharsets;

public final class MediaProjectionStreamService extends Service {
    public static final String EXTRA_RESULT_CODE = "resultCode";
    public static final String EXTRA_RESULT_DATA = "resultData";
    public static final String EXTRA_HOST = "host";
    public static final String EXTRA_PORT = "port";
    public static final String EXTRA_WIDTH = "width";
    public static final String EXTRA_HEIGHT = "height";

    private static final String TAG = "RustyXRMakepadMedia";
    private static final String CHANNEL_ID = "rusty_xr_makepad_media_projection";
    private static final int NOTIFICATION_ID = 8714;

    private HandlerThread captureThread;
    private Handler captureHandler;
    private ImageReader imageReader;
    private VirtualDisplay virtualDisplay;
    private MediaProjection mediaProjection;
    private Socket socket;
    private BufferedOutputStream stream;
    private int width;
    private int height;
    private long frameIndex;

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        Notification notification = createNotification();
        if (android.os.Build.VERSION.SDK_INT >= 29) {
            startForeground(
                NOTIFICATION_ID,
                notification,
                ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION);
        } else {
            startForeground(NOTIFICATION_ID, notification);
        }

        if (intent == null) {
            stopSelf();
            return START_NOT_STICKY;
        }

        String host = intent.getStringExtra(EXTRA_HOST);
        if (host == null || host.length() == 0) {
            host = "127.0.0.1";
        }
        int port = intent.getIntExtra(EXTRA_PORT, 8787);
        width = intent.getIntExtra(EXTRA_WIDTH, 512);
        height = intent.getIntExtra(EXTRA_HEIGHT, 288);

        MediaProjectionManager manager =
            (MediaProjectionManager) getSystemService(Context.MEDIA_PROJECTION_SERVICE);
        Intent resultData = intent.getParcelableExtra(EXTRA_RESULT_DATA);
        int resultCode = intent.getIntExtra(EXTRA_RESULT_CODE, 0);
        if (manager == null || resultData == null || resultCode == 0) {
            Log.e(TAG, "Missing MediaProjection result data");
            stopSelf();
            return START_NOT_STICKY;
        }

        captureThread = new HandlerThread("RustyXRMakepadMediaProjection");
        captureThread.start();
        captureHandler = new Handler(captureThread.getLooper());

        final String captureHost = host;
        final int capturePort = port;
        captureHandler.post(new Runnable() {
            @Override
            public void run() {
                startCapture(manager, resultCode, resultData, captureHost, capturePort);
            }
        });

        return START_STICKY;
    }

    @Override
    public void onDestroy() {
        closeQuietly();
        super.onDestroy();
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }

    private void onImageAvailable(ImageReader reader) {
        Image image = null;
        try {
            if (reader != imageReader || stream == null) {
                return;
            }
            image = reader.acquireLatestImage();
            if (image == null || stream == null) {
                return;
            }

            byte[] payload = copyTightRgba(image);
            long timestamp = image.getTimestamp();
            long index = frameIndex++;
            String header =
                "{" +
                "\"byte_len\":" + payload.length + "," +
                "\"frame_index\":" + index + "," +
                "\"timestamp_ns\":" + timestamp + "," +
                "\"width\":" + width + "," +
                "\"height\":" + height + "," +
                "\"format\":\"rgba8888\"," +
                "\"stream\":\"display_composite\"" +
                "}";

            byte[] headerBytes = header.getBytes(StandardCharsets.UTF_8);
            ByteBuffer prefix = ByteBuffer.allocate(4).order(ByteOrder.LITTLE_ENDIAN);
            prefix.putInt(headerBytes.length);
            stream.write(prefix.array());
            stream.write(headerBytes);
            stream.write(payload);
            stream.flush();

            if (index == 0 || index % 30 == 0) {
                Log.i(TAG, "MediaProjection stream frame " + index + " bytes=" + payload.length);
            }
        } catch (IOException error) {
            Log.e(TAG, "MediaProjection stream write failed", error);
            stopSelf();
        } catch (IllegalStateException error) {
            Log.w(TAG, "MediaProjection image became unavailable during shutdown", error);
            stopSelf();
        } finally {
            if (image != null) {
                try {
                    image.close();
                } catch (IllegalStateException ignored) {
                }
            }
        }
    }

    private void startCapture(
        MediaProjectionManager manager,
        int resultCode,
        Intent resultData,
        String host,
        int port) {
        try {
            socket = new Socket(host, port);
            socket.setTcpNoDelay(true);
            stream = new BufferedOutputStream(socket.getOutputStream(), 256 * 1024);

            mediaProjection = manager.getMediaProjection(resultCode, resultData);
            if (mediaProjection == null) {
                Log.e(TAG, "MediaProjection was not created");
                stopSelf();
                return;
            }
            mediaProjection.registerCallback(new MediaProjection.Callback() {
                @Override
                public void onStop() {
                    Log.i(TAG, "MediaProjection stopped by system");
                    mediaProjection = null;
                    stopSelf();
                }
            }, captureHandler);

            imageReader = ImageReader.newInstance(width, height, PixelFormat.RGBA_8888, 3);
            imageReader.setOnImageAvailableListener(new ImageReader.OnImageAvailableListener() {
                @Override
                public void onImageAvailable(ImageReader reader) {
                    MediaProjectionStreamService.this.onImageAvailable(reader);
                }
            }, captureHandler);
            virtualDisplay = mediaProjection.createVirtualDisplay(
                "Rusty XR Makepad Display Composite",
                width,
                height,
                getResources().getDisplayMetrics().densityDpi,
                DisplayManager.VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR,
                imageReader.getSurface(),
                null,
                captureHandler);

            Log.i(TAG, "MediaProjection stream started " + width + "x" + height + " -> " + host + ":" + port);
        } catch (IOException error) {
            Log.e(TAG, "Could not connect to media receiver", error);
            stopSelf();
        } catch (RuntimeException error) {
            Log.e(TAG, "MediaProjection stream setup failed", error);
            stopSelf();
        }
    }

    private byte[] copyTightRgba(Image image) {
        Image.Plane plane = image.getPlanes()[0];
        ByteBuffer buffer = plane.getBuffer();
        int rowStride = plane.getRowStride();
        int pixelStride = plane.getPixelStride();
        byte[] payload = new byte[width * height * 4];
        int dst = 0;
        for (int y = 0; y < height; y++) {
            int rowStart = y * rowStride;
            for (int x = 0; x < width; x++) {
                int src = rowStart + x * pixelStride;
                payload[dst++] = buffer.get(src);
                payload[dst++] = buffer.get(src + 1);
                payload[dst++] = buffer.get(src + 2);
                payload[dst++] = buffer.get(src + 3);
            }
        }
        return payload;
    }

    private Notification createNotification() {
        NotificationManager manager = (NotificationManager) getSystemService(NOTIFICATION_SERVICE);
        if (manager != null && android.os.Build.VERSION.SDK_INT >= 26) {
            NotificationChannel channel = new NotificationChannel(
                CHANNEL_ID,
                "Rusty XR Makepad media capture",
                NotificationManager.IMPORTANCE_LOW);
            manager.createNotificationChannel(channel);
        }

        Notification.Builder builder = android.os.Build.VERSION.SDK_INT >= 26
            ? new Notification.Builder(this, CHANNEL_ID)
            : new Notification.Builder(this);
        return builder
            .setContentTitle("Rusty XR Makepad media capture")
            .setContentText("Streaming display-composite frames to the paired receiver.")
            .setSmallIcon(android.R.drawable.presence_video_online)
            .setOngoing(true)
            .build();
    }

    private void closeQuietly() {
        if (virtualDisplay != null) {
            virtualDisplay.release();
            virtualDisplay = null;
        }
        if (imageReader != null) {
            ImageReader reader = imageReader;
            imageReader = null;
            try {
                reader.setOnImageAvailableListener(null, null);
            } catch (RuntimeException ignored) {
            }
            reader.close();
        }
        if (mediaProjection != null) {
            mediaProjection.stop();
            mediaProjection = null;
        }
        try {
            if (stream != null) {
                stream.close();
            }
        } catch (IOException ignored) {
        }
        stream = null;
        try {
            if (socket != null) {
                socket.close();
            }
        } catch (IOException ignored) {
        }
        socket = null;
        if (captureThread != null) {
            captureThread.quitSafely();
            captureThread = null;
        }
    }
}
