package com.sidewire;

import java.io.DataInputStream;
import java.io.DataOutputStream;
import java.io.EOFException;
import java.io.InputStreamReader;
import java.lang.reflect.Method;
import java.nio.charset.StandardCharsets;

public final class ClipboardHelper {
    private static final String SHELL_PACKAGE = "com.android.shell";
    private static final int MAX_TEXT_BYTES = 4 * 1024 * 1024;
    private static final int OP_GET = 1;
    private static final int OP_SET = 2;
    private static final int OP_CLEAR = 3;
    private static final int STATUS_OK = 0;
    private static final int STATUS_ERROR = 1;

    private ClipboardHelper() {}

    private static Object clipboardService() throws Exception {
        Class<?> serviceManager = Class.forName("android.os.ServiceManager");
        Object binder = serviceManager.getMethod("getService", String.class)
                .invoke(null, "clipboard");
        if (binder == null) throw new IllegalStateException("clipboard service is unavailable");
        Class<?> iBinder = Class.forName("android.os.IBinder");
        Class<?> stub = Class.forName("android.content.IClipboard$Stub");
        Method asInterface = stub.getDeclaredMethod("asInterface", iBinder);        asInterface.setAccessible(true);
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
        Class<?>[] types = method.getParameterTypes();        Object[] args = new Object[types.length];
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
        if (clip != null && !clipSet) {
            throw new IllegalStateException("ClipData argument missing");
        }
        return args;
    }

    private static Object newPlainText(String text) throws Exception {
        Class<?> clipData = Class.forName("android.content.ClipData");
        return clipData.getMethod("newPlainText", CharSequence.class, CharSequence.class)
                .invoke(null, "SideWire", text);
    }
    private static String getText(Object service, Method getPrimaryClip) throws Exception {
        Object clip = getPrimaryClip.invoke(service, argumentsFor(getPrimaryClip, null));
        if (clip == null) return "";
        Class<?> clipData = Class.forName("android.content.ClipData");
        int count = (Integer) clipData.getMethod("getItemCount").invoke(clip);
        if (count == 0) return "";
        Object item = clipData.getMethod("getItemAt", int.class).invoke(clip, 0);
        Object text = item.getClass().getMethod("getText").invoke(item);
        return text == null ? "" : text.toString();
    }

    private static void setText(Object service, Method setPrimaryClip, String text) throws Exception {
        Object clip = newPlainText(text);
        setPrimaryClip.invoke(service, argumentsFor(setPrimaryClip, clip));
    }

    private static void clear(Object service, Method clearPrimaryClip, Method setPrimaryClip)
            throws Exception {
        if (clearPrimaryClip != null) {
            clearPrimaryClip.invoke(service, argumentsFor(clearPrimaryClip, null));
        } else {
            setText(service, setPrimaryClip, "");
        }
    }

    private static String readStdin() throws Exception {
        InputStreamReader reader = new InputStreamReader(System.in, StandardCharsets.UTF_8);        StringBuilder text = new StringBuilder();
        char[] buffer = new char[8192];
        int count;
        while ((count = reader.read(buffer)) != -1) text.append(buffer, 0, count);
        return text.toString();
    }

    private static void writeResponse(DataOutputStream output, int status, byte[] payload)
            throws Exception {
        output.writeByte(status);
        output.writeInt(payload.length);
        output.write(payload);
        output.flush();
    }

    private static void writeError(DataOutputStream output, Throwable error) throws Exception {
        String message = error.getClass().getSimpleName() + ": " + String.valueOf(error.getMessage());
        byte[] payload = message.getBytes(StandardCharsets.UTF_8);
        if (payload.length > MAX_TEXT_BYTES) {
            byte[] truncated = new byte[MAX_TEXT_BYTES];
            System.arraycopy(payload, 0, truncated, 0, truncated.length);
            payload = truncated;
        }
        writeResponse(output, STATUS_ERROR, payload);
    }

    private static byte[] readPayload(DataInputStream input, int length) throws Exception {
        if (length < 0 || length > MAX_TEXT_BYTES) {
            throw new IllegalArgumentException("clipboard payload exceeds 4 MiB limit");
        }        byte[] payload = new byte[length];
        input.readFully(payload);
        return payload;
    }

    private static void runServer() throws Exception {
        Object service = clipboardService();
        Method getPrimaryClip = clipboardMethod("getPrimaryClip");
        Method setPrimaryClip = clipboardMethod("setPrimaryClip");
        Method clearPrimaryClip;
        try {
            clearPrimaryClip = clipboardMethod("clearPrimaryClip");
        } catch (NoSuchMethodException ignored) {
            clearPrimaryClip = null;
        }

        DataInputStream input = new DataInputStream(System.in);
        DataOutputStream output = new DataOutputStream(System.out);
        while (true) {
            final int operation;
            try {
                operation = input.readUnsignedByte();
            } catch (EOFException eof) {
                return;
            }
            int length = input.readInt();
            if (length < 0 || length > MAX_TEXT_BYTES) {
                writeError(output, new IllegalArgumentException("clipboard payload exceeds 4 MiB limit"));
                return;
            }
            try {
                byte[] payload = readPayload(input, length);
                switch (operation) {
                    case OP_GET:
                        if (payload.length != 0) throw new IllegalArgumentException("GET payload must be empty");                        writeResponse(output, STATUS_OK,
                                getText(service, getPrimaryClip).getBytes(StandardCharsets.UTF_8));
                        break;
                    case OP_SET:
                        setText(service, setPrimaryClip, new String(payload, StandardCharsets.UTF_8));
                        writeResponse(output, STATUS_OK, new byte[0]);
                        break;
                    case OP_CLEAR:
                        if (payload.length != 0) throw new IllegalArgumentException("CLEAR payload must be empty");
                        clear(service, clearPrimaryClip, setPrimaryClip);
                        writeResponse(output, STATUS_OK, new byte[0]);
                        break;
                    default:
                        throw new IllegalArgumentException("unknown clipboard operation: " + operation);
                }
            } catch (Throwable error) {
                writeError(output, error);
            }
        }
    }

    private static void runOneShot(String operation) throws Exception {
        Object service = clipboardService();
        switch (operation) {
            case "get":
                System.out.print(getText(service, clipboardMethod("getPrimaryClip")));
                break;
            case "set":
                setText(service, clipboardMethod("setPrimaryClip"), readStdin());
                break;            case "clear":
                Method clearPrimaryClip;
                try {
                    clearPrimaryClip = clipboardMethod("clearPrimaryClip");
                } catch (NoSuchMethodException ignored) {
                    clearPrimaryClip = null;
                }
                clear(service, clearPrimaryClip, clipboardMethod("setPrimaryClip"));
                break;
            default:
                throw new IllegalArgumentException("unknown clipboard operation: " + operation);
        }
    }

    public static void main(String[] args) {
        try {
            if (args.length < 1) throw new IllegalArgumentException("usage: get|set|clear|server");
            if (args[0].equals("server")) {
                runServer();
            } else {
                runOneShot(args[0]);
            }
        } catch (Throwable error) {
            error.printStackTrace(System.err);
            System.exit(1);
        }
    }
}
