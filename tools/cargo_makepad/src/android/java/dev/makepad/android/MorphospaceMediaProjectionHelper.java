package dev.makepad.android;

import android.app.Activity;
import android.content.Context;
import android.content.Intent;
import android.media.projection.MediaProjectionManager;
import android.os.Handler;
import android.util.Log;

final class MorphospaceMediaProjectionHelper {
    private static final String LOG_TAG = "MorphospaceMakepad";
    private static final int REQUEST_CODE = 8713;
    private static final long DEFAULT_DELAY_MS = 1600L;
    private static final String EXTRA_ENABLE = "rustyquest.makepad.mediaProjection";
    private static final String EXTRA_DELAY_MS = "rustyquest.makepad.mediaProjectionDelayMs";
    private static final String EXTRA_PORT = "rustyquest.makepad.mediaProjectionPort";
    private static final String EXTRA_WIDTH = "rustyquest.makepad.mediaProjectionWidth";
    private static final String EXTRA_HEIGHT = "rustyquest.makepad.mediaProjectionHeight";

    private final Activity mActivity;
    private final Handler mHandler;
    private final MediaProjectionManager mMediaProjectionManager;

    MorphospaceMediaProjectionHelper(Activity activity, Handler handler) {
        mActivity = activity;
        mHandler = handler;
        mMediaProjectionManager =
            (MediaProjectionManager) activity.getSystemService(Context.MEDIA_PROJECTION_SERVICE);
    }

    void requestIfEnabled(Intent intent) {
        if (!MorphospaceActivitySupport.intentBooleanExtra(intent, EXTRA_ENABLE, false)) {
            return;
        }
        long delayMs = MorphospaceActivitySupport.intentLongExtra(intent, EXTRA_DELAY_MS, DEFAULT_DELAY_MS);
        mHandler.postDelayed(new Runnable() {
            @Override
            public void run() {
                request();
            }
        }, Math.max(0L, delayMs));
    }

    boolean handleActivityResult(int requestCode, int resultCode, Intent data) {
        if (requestCode != REQUEST_CODE) {
            return false;
        }
        if (resultCode != Activity.RESULT_OK || data == null) {
            Log.w(LOG_TAG, "MediaProjection consent denied or cancelled");
            return true;
        }
        Log.i(LOG_TAG, "MediaProjection consent granted; starting stream service");
        Intent serviceIntent = new Intent(mActivity, MediaProjectionStreamService.class);
        Intent configIntent = mActivity.getIntent();
        serviceIntent.putExtra(MediaProjectionStreamService.EXTRA_RESULT_CODE, resultCode);
        serviceIntent.putExtra(MediaProjectionStreamService.EXTRA_RESULT_DATA, data);
        serviceIntent.putExtra(MediaProjectionStreamService.EXTRA_HOST, "127.0.0.1");
        serviceIntent.putExtra(
            MediaProjectionStreamService.EXTRA_PORT,
            MorphospaceActivitySupport.intentIntExtra(configIntent, EXTRA_PORT, 8787)
        );
        serviceIntent.putExtra(
            MediaProjectionStreamService.EXTRA_WIDTH,
            MorphospaceActivitySupport.intentIntExtra(configIntent, EXTRA_WIDTH, 512)
        );
        serviceIntent.putExtra(
            MediaProjectionStreamService.EXTRA_HEIGHT,
            MorphospaceActivitySupport.intentIntExtra(configIntent, EXTRA_HEIGHT, 288)
        );
        mActivity.startForegroundService(serviceIntent);
        return true;
    }

    void stopService() {
        mActivity.stopService(new Intent(mActivity, MediaProjectionStreamService.class));
    }

    private void request() {
        if (mMediaProjectionManager == null) {
            Log.w(LOG_TAG, "MediaProjectionManager is unavailable");
            return;
        }
        Log.i(LOG_TAG, "Requesting MediaProjection consent");
        mActivity.startActivityForResult(
            mMediaProjectionManager.createScreenCaptureIntent(),
            REQUEST_CODE
        );
    }
}
