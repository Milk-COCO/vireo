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

    // ---------------------------------------------------------------------
    // 生命周期转发（**别删这两行**）
    //
    // winit 的 Android 后端把 onPause / onStop 这些回调注册了，但**不发事件**——
    // MainEvent::Pause / Stop 在 Android 侧永远不会被构造。所以 vireo 收不到
    // 「我进后台了」，切后台后会继续全速渲染直到被系统杀进程。
    //
    // 这两个 native 方法由 vireo 在运行时用 RegisterNatives 绑到固定的 C 符号上，
    // 所以**方法名不能改**，但包名 / 类名随便改（绑定用的是 Activity 实例的实际类）。
    //
    // 绑不上时 vireo 会在 logcat 打一条 warn，说明照抄这段即可；
    // 绑不上时的行为 = 旧行为（后台仍全速），不会崩。
    // ---------------------------------------------------------------------
    private static native void nativeOnPause();

    private static native void nativeOnResume();

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        // false = 允许内容延伸到系统栏/刘海区域，vireo 自己算安全区。
        // 保持 true 会多出一层 inset 缩放，与 vireo 提供的三值安全区重复。
        WindowCompat.setDecorFitsSystemWindows(getWindow(), false);
        super.onCreate(savedInstanceState);
    }

    @Override
    protected void onPause() {
        // 先转给 vireo 再 super：vireo 侧会立刻停掉整条渲染 Thread
        // （不 on_tick、不 draw、不 present），早一刻是一刻。
        nativeOnPause();
        super.onPause();
    }

    @Override
    protected void onResume() {
        super.onResume();
        // 恢复放在 super 之后：此时窗口已可见，渲染线程被唤醒后画的第一帧
        // 才不会出现「黑一下再上内容」。
        nativeOnResume();
    }
}