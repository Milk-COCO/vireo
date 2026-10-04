# 本工程默认 `minifyEnabled false`（见 build.gradle 的 release 段）。
# 只有你确定要开 R8 时才需要认真填这个文件。
#
# 为什么默认关：android-activity 的 GameActivity 依赖走反射
# （`GameActivity` 由 AAR 提供，Rust 侧通过 JNI 字符串查找类/方法）。
# 混淆会改名这些类，导致运行时找不到入口 —— 而且是 release-only 的崩溃，
# debug 一切正常，非常难查。开之前先读 platform/android.rs 顶部的警告。