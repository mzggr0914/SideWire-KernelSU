package com.sidewire;

import java.io.InputStreamReader;
import java.lang.reflect.Method;
import java.nio.charset.StandardCharsets;

public final class ClipboardHelper {
    private static final String SHELL_PACKAGE = "com.android.shell";

    private ClipboardHelper() {}

    private static Object clipboardService() throws Exception {
        Class<?> serviceManager = Class.forName("android.os.ServiceManager");
        Object binder = serviceManager.getMethod("getService", String.class)
                .invoke(null, "clipboard");
        if (binder == null) throw new IllegalStateException("clipboard service is unavailable");
        Class<?> iBinder = Class.forName("android.os.IBinder");
        Class<?> stub = Class.forName("android.content.IClipboard$Stub");
        Method asInterface = stub.getDeclaredMethod("asInterface", iBinder);
        asInterface.setAccessible(true);
        return asInterface.invoke(null, binder);
    }

    private static int currentUserId() {
        try {
            Class<?> activityManager = Class.forName("android.app.ActivityManager");
            Method getCurrentUser = activityManager.getDeclaredMethod("getCurrentUser");
            getCurrentUser.setAccessible(true);
            return (Integer) getCurrentUser.invoke(null);
        } catch (Throwable ignored) {
            return 0;
        }
    }
    private static Method clipboardMethod(String name) throws Exception {
        Class<?> clipboard = Class.forName("android.content.IClipboard");
        for (Method method : clipboard.getMethods()) {
            if (method.getName().equals(name)) {
                method.setAccessible(true);
                return method;
            }
        }
        throw new NoSuchMethodException("IClipboard." + name);
    }

    private static Object[] argumentsFor(Method method, Object clip) throws Exception {
        Class<?>[] types = method.getParameterTypes();
        Object[] args = new Object[types.length];
        boolean packageSet = false;
        boolean clipSet = false;
        int intIndex = 0;
        for (int i = 0; i < types.length; i++) {
            String name = types[i].getName();
            if (name.equals("android.content.ClipData")) {
                args[i] = clip;
                clipSet = true;
            } else if (types[i] == String.class) {
                args[i] = packageSet ? null : SHELL_PACKAGE;
                packageSet = true;
            } else if (types[i] == int.class) {
                args[i] = intIndex++ == 0 ? currentUserId() : 0;
            } else {
                throw new IllegalStateException("unsupported IClipboard argument: " + name);
            }
        }
        if (clip != null && !clipSet) throw new IllegalStateException("ClipData argument missing");
        return args;
    }
    private static Object newPlainText(String text) throws Exception {
        Class<?> clipData = Class.forName("android.content.ClipData");
        return clipData.getMethod("newPlainText", CharSequence.class, CharSequence.class)
                .invoke(null, "SideWire", text);
    }

    private static String getText(Object service) throws Exception {
        Method getPrimaryClip = clipboardMethod("getPrimaryClip");
        Object clip = getPrimaryClip.invoke(service, argumentsFor(getPrimaryClip, null));
        if (clip == null) return "";
        Class<?> clipData = Class.forName("android.content.ClipData");
        int count = (Integer) clipData.getMethod("getItemCount").invoke(clip);
        if (count == 0) return "";
        Object item = clipData.getMethod("getItemAt", int.class).invoke(clip, 0);
        Object text = item.getClass().getMethod("getText").invoke(item);
        return text == null ? "" : text.toString();
    }

    private static void setText(Object service, String text) throws Exception {
        Object clip = newPlainText(text);
        Method setPrimaryClip = clipboardMethod("setPrimaryClip");
        setPrimaryClip.invoke(service, argumentsFor(setPrimaryClip, clip));
    }

    private static void clear(Object service) throws Exception {
        try {
            Method clearPrimaryClip = clipboardMethod("clearPrimaryClip");
            clearPrimaryClip.invoke(service, argumentsFor(clearPrimaryClip, null));
        } catch (NoSuchMethodException ignored) {
            setText(service, "");
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
            Object service = clipboardService();
            switch (args[0]) {
                case "get":
                    System.out.print(getText(service));
                    break;
                case "set":
                    setText(service, readStdin());
                    break;
                case "clear":
                    clear(service);
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
