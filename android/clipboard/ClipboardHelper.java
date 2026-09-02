package com.sidewire;

import java.io.InputStreamReader;
import java.lang.reflect.Method;
import java.nio.charset.StandardCharsets;

public final class ClipboardHelper {
    private ClipboardHelper() {}

    private static Object shellContext() throws Exception {
        Class<?> activityThread = Class.forName("android.app.ActivityThread");
        Method systemMain = activityThread.getDeclaredMethod("systemMain");
        systemMain.setAccessible(true);
        Object thread = systemMain.invoke(null);
        Method getSystemContext = activityThread.getDeclaredMethod("getSystemContext");
        getSystemContext.setAccessible(true);
        Object systemContext = getSystemContext.invoke(thread);
        Class<?> context = Class.forName("android.content.Context");
        Method createPackageContext = context.getMethod("createPackageContext", String.class, int.class);
        return createPackageContext.invoke(systemContext, "com.android.shell", 0);
    }

    private static Object clipboard(Object context) throws Exception {
        Class<?> contextClass = Class.forName("android.content.Context");
        Method getSystemService = contextClass.getMethod("getSystemService", String.class);
        return getSystemService.invoke(context, "clipboard");
    }
    private static String getText(Object context, Object clipboard) throws Exception {
        Class<?> manager = Class.forName("android.content.ClipboardManager");
        Object clip = manager.getMethod("getPrimaryClip").invoke(clipboard);
        if (clip == null) return "";
        Class<?> clipData = Class.forName("android.content.ClipData");
        int count = (Integer) clipData.getMethod("getItemCount").invoke(clip);
        if (count == 0) return "";
        Object item = clipData.getMethod("getItemAt", int.class).invoke(clip, 0);
        Class<?> itemClass = Class.forName("android.content.ClipData$Item");
        Class<?> contextClass = Class.forName("android.content.Context");
        Object text = itemClass.getMethod("coerceToText", contextClass).invoke(item, context);
        return text == null ? "" : text.toString();
    }

    private static void setText(Object clipboard, String text) throws Exception {
        Class<?> clipData = Class.forName("android.content.ClipData");
        Object clip = clipData.getMethod("newPlainText", CharSequence.class, CharSequence.class)
                .invoke(null, "SideWire", text);
        Class<?> manager = Class.forName("android.content.ClipboardManager");
        manager.getMethod("setPrimaryClip", clipData).invoke(clipboard, clip);
    }

    private static void clear(Object clipboard) throws Exception {
        Class<?> manager = Class.forName("android.content.ClipboardManager");
        try {
            manager.getMethod("clearPrimaryClip").invoke(clipboard);
        } catch (NoSuchMethodException ignored) {
            setText(clipboard, "");
        }
    }

    private static String readStdin() throws Exception {
        InputStreamReader reader = new InputStreamReader(System.in, StandardCharsets.UTF_8);
        StringBuilder text = new StringBuilder();
        char[] buffer = new char[8192];
        int count;
        while ((count = reader.read(buffer)) != -1) text.append(buffer, 0, count);
        return text.toString();
    }

    public static void main(String[] args) {
        try {
            if (args.length < 1) throw new IllegalArgumentException("usage: get|set|clear");
            Object context = shellContext();
            Object clipboard = clipboard(context);
            switch (args[0]) {
                case "get":
                    System.out.print(getText(context, clipboard));
                    break;
                case "set":
                    setText(clipboard, readStdin());
                    break;
                case "clear":
                    clear(clipboard);
                    break;
                default:
                    throw new IllegalArgumentException("unknown clipboard operation: " + args[0]);
            }
        } catch (Throwable error) {
            error.printStackTrace(System.err);
            System.exit(1);
        }
    }
}
