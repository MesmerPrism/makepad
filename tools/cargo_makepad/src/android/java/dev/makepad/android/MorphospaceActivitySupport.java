package dev.makepad.android;

import android.content.Intent;
import android.util.Log;

final class MorphospaceActivitySupport {
    private static final String LOG_TAG = "MorphospaceMakepad";

    private MorphospaceActivitySupport() {
    }

    static void activityMarker(String phase) {
        Log.e(
            LOG_TAG,
            "MORPHOSPACE_MAKEPAD_ANDROID_ACTIVITY schema=rusty.makepad.android_activity.v1 phase="
                + phase
                + " renderer=makepad android_packager=cargo-makepad"
        );
    }

    static boolean intentBooleanExtra(Intent intent, String key, boolean fallback) {
        if (intent == null || !intent.hasExtra(key) || intent.getExtras() == null) {
            return fallback;
        }
        Object value = intent.getExtras().get(key);
        if (value instanceof Boolean) {
            return ((Boolean) value).booleanValue();
        }
        if (value instanceof String) {
            String text = ((String) value).trim().toLowerCase();
            return "true".equals(text) || "1".equals(text) || "yes".equals(text) || "on".equals(text);
        }
        return fallback;
    }

    static int intentIntExtra(Intent intent, String key, int fallback) {
        if (intent == null || !intent.hasExtra(key) || intent.getExtras() == null) {
            return fallback;
        }
        try {
            Object value = intent.getExtras().get(key);
            if (value instanceof Number) {
                return ((Number) value).intValue();
            }
            if (value instanceof String) {
                return Integer.parseInt(((String) value).trim());
            }
        } catch (RuntimeException ignored) {
        }
        return fallback;
    }

    static long intentLongExtra(Intent intent, String key, long fallback) {
        if (intent == null || !intent.hasExtra(key) || intent.getExtras() == null) {
            return fallback;
        }
        try {
            Object value = intent.getExtras().get(key);
            if (value instanceof Number) {
                return ((Number) value).longValue();
            }
            if (value instanceof String) {
                return Long.parseLong(((String) value).trim());
            }
        } catch (RuntimeException ignored) {
        }
        return fallback;
    }
}
