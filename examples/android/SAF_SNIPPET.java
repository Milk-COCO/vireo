// vireo SAF（系统文件选择器）桥 —— 独立可复制文件。
//
// 你的项目要支持 `App::open_document_tree` / `open_document` / `create_document`，
// 把下面「复制块」里的东西拷进你自己的 Activity（`extends GameActivity` 的那个）：
//
//   块 1（import）       → 与其它 import 放一起
//   块 2（字段 + 启动方法 + native 声明） → 类体里任意位置
//   块 3（onCreate 注册） → `super.onCreate(...)` 之后
//
// 拷完不需要改包名、不需要改类名：Rust 侧用 `RegisterNatives` 绑的是
// **Activity 实例的实际类**，方法名对上就行（`vireoLaunchSaf` / `nativeOnSafResult`）。
//
// 模板的 `MainActivity.java` 里是同一份代码的已集成版，两边改了要同步。
//
// 为什么必须有这段 Java（不能纯 Rust）：
// - `android-activity` 的 native 胶水没有 Activity Result 通道，结果只能走 Java 回来；
// - `registerForActivityResult` 必须在 `onCreate` 里（STARTED 之前）注册，
//   Rust 侧的调用时机保证不了，只能 Java 侧提前注册好、Rust 只负责 launch。
//
// 为什么用 `StartActivityForResult` contract 而不用 `OpenDocumentTree` 那三个：
// - 那三个生成的 Intent 不带任何 flag（反编译 AAR 字节码确认过），`PERSISTABLE`
//   没有，授权活不过进程重启 —— 目录娶回来下次启动就用不了，等于白做。

// ---- 复制块 1：import（与其它 import 放一起）----
/*
import android.app.Activity;
import android.content.Intent;
import android.net.Uri;
import androidx.activity.result.ActivityResultLauncher;
import androidx.activity.result.contract.ActivityResultContracts;
*/

// ---- 复制块 2：字段 + 启动方法 + native 声明（类体里任意位置）----
/*
    // ---- SAF 桥（vireo `App::open_document_tree` 等需要它）----
    //
    // 单个 launcher 覆盖三种选择：Intent 由 Rust 侧按需组装（action/type/mime/title），
    // 这里只负责 UI 线程 hop + 结果回传。requestId 全程透传，Rust 侧按 id 配回调。
    private ActivityResultLauncher<Intent> safLauncher;
    private long pendingSafRequest = 0;

    /**
     * Rust 侧发起 SAF 选择（`App::open_document_tree` 等经 JNI 调这里）。
     *
     * <p>任何线程可调：内部 `runOnUiThread` 接管。单文件/新建加 `CATEGORY_OPENABLE`，
     * 选目录不加（目录本身不是可打开文档）。三个授权 flag 全要 —— 理由见文件头：
     * 缺 `PERSISTABLE` 则授权活不过重启。
     *
     * @param requestId Rust 侧的请求 id，原样随结果返回
     * @param action    如 `Intent.ACTION_OPEN_DOCUMENT_TREE`
     * @param type      mime 主类型（可 null，目录选择时为 null）
     * @param mimes     mime 过滤数组（可 null）
     * @param title     建议文件名（新建时用，可 null）
     */
    public void vireoLaunchSaf(long requestId, String action, String type, String[] mimes, String title) {
        runOnUiThread(() -> {
            try {
                Intent i = new Intent(action);
                if (type != null) {
                    i.setType(type);
                }
                if (mimes != null) {
                    i.putExtra(Intent.EXTRA_MIME_TYPES, mimes);
                }
                if (title != null) {
                    i.putExtra(Intent.EXTRA_TITLE, title);
                }
                if (!Intent.ACTION_OPEN_DOCUMENT_TREE.equals(action)) {
                    i.addCategory(Intent.CATEGORY_OPENABLE);
                }
                i.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION
                        | Intent.FLAG_GRANT_WRITE_URI_PERMISSION
                        | Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION);
                pendingSafRequest = requestId;
                safLauncher.launch(i);
            } catch (Exception e) {
                // 如设备上没有 DocumentsUI：转成异步错误回 Rust，不让 UI 线程崩。
                nativeOnSafResult(requestId, null, "launch failed: " + e);
            }
        });
    }

    /**
     * 选择结果回传 Rust（`vireo_android_on_saf_result`，`RegisterNatives` 绑定）。
     *
     * <p>约定：`(uri != null)` = 成功；`(uri == null, error == null)` = 用户取消；
     * `(uri == null, error != null)` = 失败。重建后迟到的旧结果 id 对不上，
     * Rust 侧丢弃 + warn。
     */
    private static native void nativeOnSafResult(long requestId, String uriOrNull, String errorOrNull);
*/

// ---- 复制块 3：onCreate 注册（`super.onCreate(...)` 之后）----
/*
        safLauncher = registerForActivityResult(
                new ActivityResultContracts.StartActivityForResult(),
                result -> {
                    long id = pendingSafRequest;
                    pendingSafRequest = 0;
                    Uri uri = null;
                    String error = null;
                    if (result.getResultCode() != Activity.RESULT_OK) {
                        // 用户点了返回：uri/error 全 null，Rust 侧按 UserCancelled 处理
                    } else if (result.getData() == null || result.getData().getData() == null) {
                        error = "picker returned no document";
                    } else {
                        uri = result.getData().getData();
                    }
                    nativeOnSafResult(id, uri == null ? null : uri.toString(), error);
                });
*/
