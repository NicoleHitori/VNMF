# SPDX-License-Identifier: GPL-3.0-or-later
# UniFFI bindings call Rust through JNA, and JNA looks up classes, fields and methods by name
# (Structure, Callback, Library) — renaming or removal by R8 would break them.
-keep class com.sun.jna.** { *; }
-keepclassmembers class * extends com.sun.jna.** { public *; }
-keep class uniffi.** { *; }
# JNA contains desktop code (AWT) — it doesn't exist on Android and is never called.
-dontwarn java.awt.**
