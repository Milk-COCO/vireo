package com.example.vireoandroid;

import android.os.Bundle;

import androidx.core.view.WindowCompat;

import com.google.androidgamesdk.GameActivity;

/**
 * 整个 Android 宿主就这么大。
 *
 * <p>为什么必须有这个类、且必须直接 {@code extends GameActivity}：android-activity
 * 的 README 明确说无法只用 Rust/JNI 渲染 Activity；vireo 的
 * {@code platform::android} 又需要在 {@code onCreate} 里用
 * {@code WindowInsetsControllerCompat} 初始化沉浸式系统栏，而 AndroidX 类没法
 * 从纯 Rust 侧创建 —— 从零手写 Java 壳太麻烦。
 *
 * <p>{@code System.loadLibrary} 的名字必须和 {@code build.gradle} 的
 * {@code rustCrate} 以及 {@code Cargo.toml} 的 {@code [lib] name} 三处一致。
 */
public class MainActivity extends GameActivity {

    static {
        // 加载 Rust cdylib。android-activity 的构建脚本已经把同名的 .so
        // 动态打进了 cdylib；加载后 AGSK 会找到并调用 Rust 导出的 android_main。
        System.loadLibrary("vireoandroid");
    }

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        // false = 允许内容延伸到系统栏/刘海区域，vireo 自己算安全区。
        // 保持 true 会多出一层 inset 缩放，与 vireo 提供的三值安全区重复。
        WindowCompat.setDecorFitsSystemWindows(getWindow(), false);
        super.onCreate(savedInstanceState);
    }
}