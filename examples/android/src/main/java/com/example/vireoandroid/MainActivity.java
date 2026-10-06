package com.example.vireoandroid;

import android.os.Build;
import android.os.Bundle;
import android.window.OnBackInvokedDispatcher;

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
    // 生命周期转发（**别删这三行**）
    //
    // winit 的 Android 后端把 onPause / onStop 这些回调注册了，但**不发事件**——
    // MainEvent::Pause / Stop 在 Android 侧永远不会被构造。所以 vireo 收不到
    // 「我进后台了」，切后台后会继续全速渲染直到被系统杀进程。
    //
    // 这三个 native 方法由 vireo 在运行时用 RegisterNatives 绑到固定的 C 符号上，
    // 所以**方法名不能改**，但包名 / 类名随便改（绑定用的是 Activity 实例的实际类）。
    //
    // nativeOnStop 是可选的：不声明 / 不调用时只是 `App::on_stopped` 永不触发，
    // 其它生命周期行为不受影响。绑不上时 vireo 会在 logcat 打一条 warn；
    // 绑不上时的行为 = 旧行为（后台仍全速），不会崩。
    // ---------------------------------------------------------------------
    private static native void nativeOnPause();

    private static native void nativeOnResume();

    private static native void nativeOnStop();

    private static native void nativeOnDestroy();

    /**
     * 返回键。**可选** —— 不声明 / 不调用时 {@code App::on_back} 永不触发，
     * 返回键退回 Android 默认行为（退出 Activity），其余生命周期行为不受影响。
     *
     * <p>为什么必须走 {@link OnBackPressedDispatcher} 而不是 onKeyDown：
     * Android 13+ 已弃用按键分发的返回键拦截，必须用 dispatcher；而
     * <b>软键盘弹出时</b>硬件键会被 IME 吃掉（喂给 InputConnection），
     * winit 的 KeyboardInput 根本收不到 —— 所以「在 on_key_down 里判
     * BrowserBack」这条路在文本输入场景下不可行。
     */
    private static native void nativeOnBackPressed();

    /** 渲染线程对「本次返回键」的处置：true = 已消费，别退出。 */
    private static native boolean nativeTakeBackConsumed();

    /**
     * 本次返回键是否已跑过 {@link #onBackPressedCompat()}。
     *
     * <p>有了 manifest 的 {@code enableOnBackInvokedCallback="true"}，33+ 只走
     * dispatcher、31/32 只走 {@code onBackPressed} 覆写 —— 两者互斥，理论上
     * 每次返回键只会进来一次。保留这个标志是**防御**：若将来某个 ROM 在两条路
     * 之间重复派发，没有它就会「一次按键退两次 + 多发一次 nativeOnBackPressed」。
     *
     * <p>复位在 {@code onResume}（不是方法末尾），理由见那里。
     */
    private boolean backDispatched = false;

    /**
     * 拦住返回键后，给渲染线程跑 {@code on_back} 回调并表态的时限（毫秒）。
     *
     * <p>为什么必须给这个窗口：UI 线程读到的「是否已消费」只可能来自**上一帧**
     * 渲染线程跑过的回调，所以本次返回键必须**先被拦住**，等渲染线程表态后才能
     * 决定退不退出。
     *
     * <p>取 34ms（60Hz 下约 2 帧）而不是 1 帧 16ms：部分设备 30fps，一帧就
     * 是 33ms，16ms 会来不及表态而误退出。失败方向是安全的 —— 超时按「未消费」
     * 退出，不会把用户困住。
     */
    private static final long BACK_DECIDE_DELAY_MS = 34;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        // false = 允许内容延伸到系统栏/刘海区域，vireo 自己算安全区。
        // 保持 true 会多出一层 inset 缩放，与 vireo 提供的三值安全区重复。
        WindowCompat.setDecorFitsSystemWindows(getWindow(), false);

        // 返回键接管。必须在 super.onCreate 之后（dispatcher 已就绪）。
        // PRIORITY_DEFAULT = 0：不抢占系统级 back（桌面模式 / 手势导航的
        // 预测性返回动画由系统负责）。
        super.onCreate(savedInstanceState);

        // ⚠️ OnBackInvokedDispatcher 是 **API 33**（Android 13）才有的，而模板
        // minSdk = 31（GameActivity 4.4.0 的下限，降不了）。所以必须分档：
        //
        //   API >= 33：走 dispatcher（同时覆盖手势导航的预测性返回）
        //   API 31/32：退回 onBackPressed() 覆写 —— 它在 API 33 上已弃用但
        //              **仍会被调用**（只要 app 没开 enableOnBackInvokedCallback），
        //              所以两条路可以并存，用 Build.VERSION 互斥即可。
        //
        // 漏掉这个分档的后果是 Android 12 设备上 NoClassDefFoundError 直接崩。
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            getOnBackInvokedDispatcher().registerOnBackInvokedCallback(
                    OnBackInvokedDispatcher.PRIORITY_DEFAULT,
                    this::onBackPressedCompat);
        }
    }

    /**
     * API 31/32 的返回键入口。
     *
     * <p>本模板的 manifest 声明了 {@code android:enableOnBackInvokedCallback="true"}，
     * 于是 <b>API 33+ 上这个覆写彻底不会被调用</b>（系统完全交给 dispatcher）。
     * 31/32 上则没有 dispatcher，必须靠它 —— 这就是下面这个分档存在的全部理由。
     *
     * <p>不做「两路并存 + 去重」而是彻底分档：两路并存的前提是
     * {@code enableOnBackInvokedCallback} 没开，而开了它才能在 33+ 收到返回键
     * —— 也就是说「33+ 想工作」和「33+ 走 legacy」互斥，只能选一个，
     * 那就选有文档、行为确定的 dispatcher。
     */
    @Override
    public void onBackPressed() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            // 33+ 由 dispatcher 负责，这里只吞掉以免重复。
            return;
        }
        onBackPressedCompat();
    }

    /**
     * 返回键的实际处置。
     *
     * <p>顺序不能反：先问渲染线程「消费了没有」，再登记「本次有返回键」。
     * 原因见 {@code android_lifecycle.rs::vireo_android_on_back_pressed} ——
     * 渲染线程在上一帧才有机会写入「已消费」，本次的答案要用上一帧的意愿。
     *
     * <p>为什么必须调 {@code nativeTakeBackConsumed()} 而不能只读：
     * 「读即清零」—— 否则上一次残留的 true 会把**下一次**返回键也吞掉，
     * 用户第二次想退出却退不出。
     */
    private void onBackPressedCompat() {
        if (backDispatched) {
            // 同一次返回键已被 dispatcher 跑过（防御，理论上 33+/31-32 互斥）。
            return;
        }
        backDispatched = true;

        // ⚠️ **不能**在这里同步读「是否已消费」然后 finish —— 那个值只能来自
        // **上一帧**渲染线程跑过的 on_back 回调，而第一次按返回键时根本不存在
        // 「上一帧」，必然读到 false，于是每次**第一次**返回键都会直接退出。
        // （第一版就是这么写的，真机实测：按一次返回键 App 就没了，二段确认的
        // 第二段永远按不到。）
        //
        // 正确做法是 Android 社区的标准方案：**先无条件拦住，再延后判定** ——
        // 给渲染线程一帧的时间去跑 on_back 回调并表态。
        nativeOnBackPressed();
        getWindow().getDecorView().postDelayed(this::finishIfNotConsumed, BACK_DECIDE_DELAY_MS);
    }

    /**
     * 延后判定：渲染线程已跑完 {@code on_back} 回调，此刻读它的表态。
     *
     * <p>读即清零（见 {@code take_back_consumed}）—— 恰好每次返回键各读一次。
     *
     * <p>{@link #backDispatched} 在**这里**复位（不是 onResume）：第一次返回键
     * 被拦住时 activity 根本没重走 onResume，若只在 onResume 复位，它会永远停在
     * true → **第二次返回键被吞掉、用户退不出**。
     *
     * <p>代价：退出有 {@link #BACK_DECIDE_DELAY_MS} 的延迟（人感知不到）。
     * 反过来若渲染线程卡住超过这个时长还没表态，就退回 Android 默认行为（退出）
     * —— 这是安全的失败方向（不会把用户困在 App 里）。
     */
    private void finishIfNotConsumed() {
        boolean consumed = nativeTakeBackConsumed();
        backDispatched = false;
        if (!consumed) {
            finish();
        }
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

    @Override
    protected void onStop() {
        // 与 onPause 的区别：onStop 只在 activity **不可见**时触发
        // （半透明弹窗 / 权限对话框只走 onPause）。vireo 用它区分
        // 「短暂中断」与「彻底离开」—— App::on_stopped 只对走过这里的暂停触发。
        //
        // 顺序与 onPause 一致：先转给 vireo 再 super。
        nativeOnStop();
        super.onStop();
    }

    @Override
    protected void onDestroy() {
        // 与前三个的区别：**只在用户主动关闭时触发**（划掉最近任务 / finish()）。
        // 按 Home、切 app、锁屏、系统回收内存**都不会**走到这里 —— 所以它
        // 回答的是「用户是切走了还是把我关了」，其余三者都回答不了。
        //
        // ⚠️ 进程被系统杀掉时**不会**触发（Android 措辞是 "might be called"），
        // 所以不能拿它做资源清理或存档 —— 要存档在 onPause 之类更早的时机做。
        //
        // 典型用途：配合 App::set_auto_pause(false)，后台照跑逻辑时靠
        // App::is_destroyed() 得知该退出了。
        nativeOnDestroy();
        super.onDestroy();
    }
}