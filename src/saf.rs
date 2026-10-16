//! Storage Access Framework（系统文件选择器）——安卓独有。
//!
//! # 为什么需要它
//!
//! [`data_dir`](crate::App::data_dir) /
//! [`external_data_dir`](crate::App::external_data_dir) 都**随卸载删除**；
//! Android 11+ 分区存储不允许往共享位置写路径。要「让用户自己选位置存/取、
//! 且卸载后文件还在」，必须走 SAF。
//!
//! # 三个入口（全异步）
//!
//! 系统选择器出现后，用户操作完才有结果，所以三个入口都**立即返回**、结果走回调：
//!
//! - [`App::open_document_tree`](crate::App::open_document_tree)：选目录 →
//!   拿到目录 URI 后相对路径读写（存档目录 / 谱面库这类长期需求）。
//! - [`App::open_document`](crate::App::open_document)：选单个已有文件 → 读。
//! - [`App::create_document`](crate::App::create_document)：选保存位置 → 写。
//!
//! 同一时刻只允许**一个**选择器在途（选择器是模态的，第二个根本没机会交互）。
//! 重叠发起直接返回 `Err`，不会静默串回调。
//!
//! # 返回的是 URI，不是路径
//!
//! SAF 不返回文件路径，只返回 `content://` URI —— `std::fs` 读不了它，
//! 也塞不进任何 `PathBuf` 参数。所以 [`SafUri`] 同时给两者：
//!
//! - URI 字符串：可持久化（自己存到 [`data_dir`](crate::App::data_dir），
//!   下次启动 [`from_uri_string`](SafUri::from_uri_string) 复用）；
//! - 开 fd 的方法：[`open_reader`](SafUri::open_reader) /
//!   [`open_writer`](SafUri::open_writer) 返回真正的 `std::fs::File`，
//!   `read_to_end` / `BufReader` / `Seek` 全可用。
//!
//! # Java 侧是强制的
//!
//! native 胶水层（`android-activity`）没有 Activity Result 通道，结果只能经
//! 用户 `MainActivity.java` 回传。模板的 `MainActivity.java` 已含完整 SAF 桥，
//! 另有独立可复制文件 `examples/android/SAF_SNIPPET.java` —— 用户项目把那段
//! 拷进自己的 Activity 即可。Java 块缺失时 launch 直接返回 `Err`（会明说缺什么），
//! 不会静默无事发生。
//!
//! 为什么不用 `OpenDocumentTree` / `OpenDocument` / `CreateDocument` 那三个
//! contract：反编译 AAR 字节码确认过，它们生成的 Intent **不带任何 flag**——
//! 连 `PERSISTABLE` 都没有，`takePersistableUriPermission` 必然抛
//! `SecurityException`，授权活不过进程重启。所以用 `StartActivityForResult`
//! contract + 自己组 Intent（现代 API + 完整 flag 控制 + requestId 全程透传）。
//!
//! # 线程模型
//!
//! launch 可在任何线程调（Java 侧 `runOnUiThread` 接管线程 hop）；结果在 UI
//! 线程到达，经 native 桥锁存，渲染线程每帧 drain 后调你的回调 ——
//! **回调跑在渲染线程**，里面可以直接用 `App` / `DrawBatch`。
//!
//! # 持久化
//!
//! 收到 URI 当即 `takePersistableUriPermission`（目录=读写，单文件读=只读，
//! 新建=读写）。失败（某些 provider 不支持）降级为本次会话有效 + `warn`，
//! 不会让整个 pick 失败。[`SafUri::persisted`] 列出历史授权，[`release`](SafUri::release)
//! 释放。卸载后授权清零是系统行为，但文件本身还在 —— 「卸载后还在」指文件，不指授权。
//!
//! # provider 差异
//!
//! 目录/文件语义只对本地存储（ExternalStorageProvider）保证。云盘等第三方
//! provider 可能缺某些操作（列目录、建文件、持久化任一种），失败会如实透出为
//! `Err`，调用方按普通 IO 失败处理即可.

//! 文件级 `dead_code` 豁免（仅非安卓）：JNI 部分整个不存在，状态机在桌面
//! 只有单测碰 —— 与 `android_lifecycle` 的模块级豁免同款理由。
#![cfg_attr(not(target_os = "android"), allow(dead_code))]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::VireoError;

#[cfg(target_os = "android")]
use crate::platform::android::{JRes, JniErr, with_activity};
#[cfg(target_os = "android")]
use jni::{jni_sig, jni_str};
#[cfg(target_os = "android")]
use std::os::unix::io::FromRawFd;

// 桌面上 JNI 部分整个不存在，未使用的状态机用例由文件级 allow 兜住
//（与 `android_lifecycle` 同款处理；单测仍在桌面跑纯逻辑部分）。
/// SAF 授权的三种形态（仅内部区分：决定 Intent 组装与持久化 flag）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SafKind {
    Tree,
    Open,
    Create,
}

/// SAF 选择结果回调：`Ok(SafUri)` = 用户选定了，`Err(UserCancelled)` = 用户点了返回。
pub(crate) type SafCallback = Box<dyn FnOnce(Result<SafUri, VireoError>) + Send + 'static>;

/// 用户选定的文档/目录。核心只是一个 `content://` URI 字符串。
///
/// - 跨启动复用：把 [`uri`](SafUri::uri) 存进 [`data_dir`](crate::App::data_dir），
///   下次 [`from_uri_string`](SafUri::from_uri_string) 拿回来（前提是授权被持久化过，
///   见模块文档；没持久化就只是本次会话有效）。
/// - 读写：[`open_reader`](SafUri::open_reader) /
///   [`open_writer`](SafUri::open_writer) 开出真正的 `std::fs::File`。
/// - 目录操作（`list` / `resolve` / `create_*` / `delete`）要求它是**目录 URI**
///   （`open_document_tree` 给的，或复用时存的就是目录 URI；单文件 URI 调这些
///   直接 `Err`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SafUri {
    uri: String,
    is_tree: bool,
}

/// [`SafUri::list`] 的一行：名字 + 是否目录。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SafEntry {
    pub name: String,
    pub is_dir: bool,
}

/// [`SafUri::persisted`] 的一行：历史授权。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SafGrant {
    pub uri: SafUri,
    pub readable: bool,
    pub writable: bool,
}

impl SafUri {
    /// 从 URI 字符串恢复（跨启动复用的入口）。
    ///
    /// 不做任何校验 —— 合法性在第一次使用时由系统判定。`is_tree` 按 `/tree/`
    /// 路段启发式判定（`DocumentsProvider` 的目录 URI 恒含该段）；判错了也不怕，
    /// 目录操作在调用时本来就会失败并明说。
    pub fn from_uri_string(s: impl Into<String>) -> Self {
        let uri = s.into();
        let is_tree = uri.contains("/tree/");
        Self { uri, is_tree }
    }

    /// 底层 `content://` URI。
    pub fn uri(&self) -> &str {
        &self.uri
    }

    /// 是否目录 URI（启发式，见 [`from_uri_string`](SafUri::from_uri_string)）。
    pub fn is_tree(&self) -> bool {
        self.is_tree
    }

    /// 以只读打开，返回真正的 `std::fs::File`。
    ///
    /// 经 `ContentResolver.openFileDescriptor(uri, "r")` → `detachFd()` →
    /// `File::from_raw_fd`，fd 所有权归这个 `File`（`drop` 即关）。
    /// 适用：`open_document` 拿到的、目录里 `resolve` 出来的。
    pub fn open_reader(&self) -> Result<std::fs::File, VireoError> {
        #[cfg(not(target_os = "android"))]
        {
            let _ = self;
            Err(VireoError::Unsupported("SAF file open"))
        }
        #[cfg(target_os = "android")]
        {
            open_fd_impl(self, "r")
        }
    }

    /// 以截断写打开，返回真正的 `std::fs::File`。
    ///
    /// 模式 `"w"`：从头写（`create_document` 新建的本来就是空文件）。
    /// 不提供追加模式 —— DocumentsProvider 的 `"wa"` 各家行为不一致，
    /// 需要追加请读出来改完整个重写。
    pub fn open_writer(&self) -> Result<std::fs::File, VireoError> {
        #[cfg(not(target_os = "android"))]
        {
            let _ = self;
            Err(VireoError::Unsupported("SAF file open"))
        }
        #[cfg(target_os = "android")]
        {
            open_fd_impl(self, "w")
        }
    }

    /// 该文档的显示名（`DocumentsContract` 的 `DISPLAY_NAME` 列）。
    pub fn display_name(&self) -> Result<String, VireoError> {
        #[cfg(not(target_os = "android"))]
        {
            let _ = self;
            Err(VireoError::Unsupported("SAF metadata query"))
        }
        #[cfg(target_os = "android")]
        {
            meta_impl(self).map(|(name, _)| name)
        }
    }

    /// 该文档的字节数。`None` = provider 不知道（云盘常见，返回 -1）。
    pub fn size(&self) -> Result<Option<u64>, VireoError> {
        #[cfg(not(target_os = "android"))]
        {
            let _ = self;
            Err(VireoError::Unsupported("SAF metadata query"))
        }
        #[cfg(target_os = "android")]
        {
            meta_impl(self).map(|(_, size)| size)
        }
    }

    /// 列出目录 `rel` 下的条目（不递归）。`""` = 目录自身。
    ///
    /// 顺序不保证（provider 相关）。
    pub fn list(&self, rel: &str) -> Result<Vec<SafEntry>, VireoError> {
        #[cfg(not(target_os = "android"))]
        {
            let _ = (self, rel);
            Err(VireoError::Unsupported("SAF directory list"))
        }
        #[cfg(target_os = "android")]
        {
            list_impl(self, rel)
        }
    }

    /// 把相对路径解成文档 URI（`read("levels/01/chart.json")` 这类用法的前置步骤）。
    ///
    /// 中间分量必须是目录；末分量文件/目录均可。`..` 拒绝（与 `Assets` 同规则）。
    pub fn resolve(&self, rel: &str) -> Result<SafUri, VireoError> {
        #[cfg(not(target_os = "android"))]
        {
            let _ = (self, rel);
            Err(VireoError::Unsupported("SAF path resolve"))
        }
        #[cfg(target_os = "android")]
        {
            resolve_impl(self, rel)
        }
    }

    /// 在目录 `rel_dir` 下建文件，返回它的文档 URI（之后 `open_writer` 写）。
    ///
    /// `mime` 如 `"application/json"`；`rel_dir` 为 `""` 表示目录自身。
    /// 中间目录必须已存在（不自动建，需要先 `create_dir` 逐级建）。
    pub fn create_file(&self, rel_dir: &str, mime: &str, name: &str) -> Result<SafUri, VireoError> {
        #[cfg(not(target_os = "android"))]
        {
            let _ = (self, rel_dir, mime, name);
            Err(VireoError::Unsupported("SAF document create"))
        }
        #[cfg(target_os = "android")]
        {
            create_impl(self, rel_dir, mime, name)
        }
    }

    /// 在目录 `rel_dir` 下建子目录。
    pub fn create_dir(&self, rel_dir: &str, name: &str) -> Result<SafUri, VireoError> {
        #[cfg(not(target_os = "android"))]
        {
            let _ = (self, rel_dir, name);
            Err(VireoError::Unsupported("SAF directory create"))
        }
        #[cfg(target_os = "android")]
        {
            const MIME_DIR: &str = "vnd.android.document/directory";
            create_impl(self, rel_dir, MIME_DIR, name).map(|mut u| {
                u.is_tree = false;
                u
            })
        }
    }

    /// 删除 `rel` 指向的文档（文件；空目录一般也可，provider 相关）。
    pub fn delete(&self, rel: &str) -> Result<(), VireoError> {
        #[cfg(not(target_os = "android"))]
        {
            let _ = (self, rel);
            Err(VireoError::Unsupported("SAF document delete"))
        }
        #[cfg(target_os = "android")]
        {
            delete_impl(self, rel)
        }
    }

    /// 手动补一次持久化授权（收到时已自动做过一次，见模块文档）。
    ///
    /// `write` = 同时要写授权（目录/新建文件传 `true`，只读打开传 `false`；
    /// 要了没被授予的权限会抛 `SecurityException`，这里吞掉转 `warn`）。
    /// 失败降级为会话有效 + `warn`，不返回 `Err` —— URI 本次会话仍可用。
    pub fn take_persistable(&self, write: bool) -> Result<(), VireoError> {
        #[cfg(not(target_os = "android"))]
        {
            let _ = (self, write);
            Err(VireoError::Unsupported("SAF persistable permission"))
        }
        #[cfg(target_os = "android")]
        {
            let Some(app) = crate::app::android_app() else {
                return Err(VireoError::Other(
                    "AndroidApp unavailable: SAF needs the #[vireo::main] android_main entry"
                        .to_string(),
                ));
            };
            let vm = unsafe { jni::JavaVM::from_raw(app.vm_as_ptr() as *mut jni::sys::JavaVM) };
            let _ = vm.attach_current_thread(|env| {
                persist_take(env, &self.uri, write);
                Ok::<(), jni::errors::Error>(())
            });
            Ok(())
        }
    }

    /// 释放对该 URI 的持久化授权（卸载/不再用时）。
    ///
    /// 只清授权记录，不删文件。
    pub fn release(&self) -> Result<(), VireoError> {
        #[cfg(not(target_os = "android"))]
        {
            let _ = self;
            Err(VireoError::Unsupported("SAF persistable permission"))
        }
        #[cfg(target_os = "android")]
        {
            release_impl(&self.uri)
        }
    }

    /// 列出系统里还记着的持久化授权（跨启动恢复的前置步骤）。
    ///
    /// 逐个 `SafUri::from_uri_string(&g.uri)` 即可复用；读/写权限见各行的 flag。
    pub fn persisted() -> Result<Vec<SafGrant>, VireoError> {
        #[cfg(not(target_os = "android"))]
        {
            Err(VireoError::Unsupported("SAF persistable permission"))
        }
        #[cfg(target_os = "android")]
        {
            persisted_impl()
        }
    }
}

// ---- 请求状态机（全平台编译；JNI 部分只在安卓） -------------------------------
// 单 Activity 进程级状态（安卓只支持单窗口，见 App::window 的守卫）。
// 与 `android_lifecycle` 的锁存同款：UI 线程写，渲染线程每帧 drain。

struct Pending {
    id: u64,
    kind: SafKind,
    cb: SafCallback,
}

struct SafState {
    pending: Option<Pending>,
    results: VecDeque<(SafCallback, Result<SafUri, VireoError>)>,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static STATE: parking_lot::Mutex<SafState> = parking_lot::Mutex::new(SafState {
    pending: None,
    results: VecDeque::new(),
});
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// 结果代号：渲染线程每帧对比，变了才 drain（与生命周期 `generation()` 同款)。
pub(crate) fn saf_generation() -> u64 {
    GENERATION.load(Ordering::Acquire)
}

/// 发起一次选择。同步 `Err` = 请求没发出去（非安卓 / 已有在途 / Java 桥缺失）；
/// 用户取消等异步结果走回调。
pub(crate) fn request(
    kind: SafKind,
    action: &str,
    typ: Option<&str>,
    mimes: &[&str],
    title: Option<&str>,
    cb: SafCallback,
) -> Result<(), VireoError> {
    #[cfg(not(target_os = "android"))]
    {
        let _ = (kind, action, typ, mimes, title, cb);
        Err(VireoError::Unsupported("SAF document picker"))
    }
    #[cfg(target_os = "android")]
    {
        request_android(kind, action, typ, mimes, title, cb)
    }
}

/// 丢掉在途请求（回调永不触发）。只用于「Activity 中途重建、结果永远回不来」
/// 的逃生口；迟到的结果到达时按未知 id 丢弃 + `warn`。
/// 返回值 = 是否真丢掉过东西。
pub(crate) fn cancel_pending() -> bool {
    STATE.lock().pending.take().is_some()
}

/// 取出全部待交付结果（渲染线程每帧调）。
pub(crate) fn drain_results() -> Vec<(SafCallback, Result<SafUri, VireoError>)> {
    STATE.lock().results.drain(..).collect()
}

#[cfg(test)]
pub(crate) fn reset_for_tests() {
    *STATE.lock() = SafState {
        pending: None,
        results: VecDeque::new(),
    };
}

// ---- 相对路径归一化（纯逻辑，全平台可测） -------------------------------------

/// 切分相对路径：分隔符两种都接受，空/`./`跳过，`..`拒绝。
/// 返回空 vec 表示根自身（`""` 合法，与 `Assets::normalize` 不同 —— 那边空串是非法名）。
pub(crate) fn normalize_rel(rel: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    for seg in rel.split(['/', '\\']) {
        if seg.is_empty() || seg == "." {
            continue;
        }
        if seg == ".." {
            return None;
        }
        out.push(seg.to_string());
    }
    Some(out)
}

// ---- 安卓实现 -----------------------------------------------------------------

#[cfg(target_os = "android")]
fn request_android(
    kind: SafKind,
    action: &str,
    typ: Option<&str>,
    mimes: &[&str],
    title: Option<&str>,
    cb: SafCallback,
) -> Result<(), VireoError> {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    {
        let mut st = STATE.lock();
        if st.pending.is_some() {
            return Err(VireoError::Other(
                "a SAF pick is already in flight; wait for its callback before starting another \
                 (or App::saf_cancel_pending if you know the picker will never return)"
                    .to_string(),
            ));
        }
        st.pending = Some(Pending { id, kind, cb });
    }
    let r = jni_launch(id, action, typ, mimes, title);
    if let Err(e) = r {
        // launch 没发出去 → 不会有结果回来，别留僵尸 pending 把后面的请求全堵死。
        STATE.lock().pending = None;
        return Err(e);
    }
    Ok(())
}

#[cfg(target_os = "android")]
fn jni_launch(
    id: u64,
    action: &str,
    typ: Option<&str>,
    mimes: &[&str],
    title: Option<&str>,
) -> Result<(), VireoError> {
    use jni::objects::{JObject, JValue};
    let app = crate::app::android_app().ok_or_else(|| {
        VireoError::Other(
            "AndroidApp unavailable: SAF needs the #[vireo::main] android_main entry".to_string(),
        )
    })?;
    with_activity(app, |env, activity| {
        let action = env.new_string(action).map_err(JniErr::from)?;
        let typ_obj = match typ {
            Some(t) => env.new_string(t).map_err(JniErr::from)?.into(),
            None => JObject::null(),
        };
        let mimes_arr = if mimes.is_empty() {
            JObject::null()
        } else {
            let arr = env
                .new_object_array(
                    mimes.len() as i32,
                    jni_str!("java/lang/String"),
                    JObject::null(),
                )
                .map_err(JniErr::from)?;
            for (i, m) in mimes.iter().enumerate() {
                let js = env.new_string(m).map_err(JniErr::from)?;
                arr.set_element(env, i, &js).map_err(JniErr::from)?;
                env.delete_local_ref(js);
            }
            arr.into()
        };
        let title_obj = match title {
            Some(t) => env.new_string(t).map_err(JniErr::from)?.into(),
            None => JObject::null(),
        };
        let r = env.call_method(
            activity,
            jni_str!("vireoLaunchSaf"),
            jni_sig!(
                "(JLjava/lang/String;Ljava/lang/String;[Ljava/lang/String;Ljava/lang/String;)V"
            ),
            &[
                JValue::Long(id as i64),
                JValue::Object(&action.into()),
                JValue::Object(&typ_obj),
                JValue::Object(&mimes_arr),
                JValue::Object(&title_obj),
            ],
        );
        // 异常卫生：抛了 Java 异常就清掉，别毒化本线程后续 JNI。
        if let Err(jni::errors::Error::JavaException) = &r {
            let _ = env.exception_clear();
        }
        r.map(|_| ()).map_err(JniErr::from)?;
        Ok(())
    })
    .map_err(|e| {
        VireoError::Other(format!(
            "SAF launch failed ({e:?}); does your MainActivity.java contain the SAF block \
             from examples/android/SAF_SNIPPET.java?"
        ))
    })
}

/// Java 侧选择器结果的 native 实现（`nativeOnSafResult`）。
///
/// 跑在 UI 线程（已 attach，这里只是 `GetEnv`）。约定：
/// - `(Some(uri), _)` → 成功（当场做持久化，见模块文档）；
/// - `(None, None)` → 用户取消；
/// - `(None, Some(err))` → Java 侧失败（如 launch 抛异常）。
/// id 对不上 pending（Activity 重建后旧结果迟到）→ 丢弃 + `warn`，不炸。
#[cfg(target_os = "android")]
pub extern "system" fn vireo_android_on_saf_result(
    _env: jni::EnvUnowned,
    _class: jni::objects::JClass<'_>,
    id: jni::sys::jlong,
    uri: jni::objects::JString<'_>,
    err: jni::objects::JString<'_>,
) {
    let Some(app) = crate::app::android_app() else {
        return;
    };
    let vm = unsafe { jni::JavaVM::from_raw(app.vm_as_ptr() as *mut jni::sys::JavaVM) };
    let _ = vm.attach_current_thread(|env| on_saf_result(env, id as u64, &uri, &err));
}

#[cfg(target_os = "android")]
fn on_saf_result(
    env: &mut jni::Env,
    id: u64,
    uri: &jni::objects::JString,
    err: &jni::objects::JString,
) -> Result<(), JniErr> {
    let uri = opt_java_string(env, uri)?;
    let err = opt_java_string(env, err)?;
    let mut st = STATE.lock();
    let Some(pending) = st.pending.take_if(|p| p.id == id) else {
        log::warn!(
            "vireo: SAF result for unknown request {id} (stale after activity recreation?) — dropped"
        );
        return Ok(());
    };
    let outcome = match (uri, err) {
        (Some(u), _) => {
            persist_best_effort(env, &u, pending.kind);
            Ok(SafUri {
                is_tree: pending.kind == SafKind::Tree,
                uri: u,
            })
        }
        (None, None) => Err(VireoError::UserCancelled),
        (None, Some(e)) => Err(VireoError::Other(e)),
    };
    st.results.push_back((pending.cb, outcome));
    GENERATION.fetch_add(1, Ordering::Release);
    Ok(())
}

#[cfg(target_os = "android")]
fn opt_java_string(
    env: &mut jni::Env,
    s: &jni::objects::JString,
) -> Result<Option<String>, JniErr> {
    if s.as_raw().is_null() {
        return Ok(None);
    }
    // jni 0.22：`Env::get_string` 已弃用，走 `JString::try_to_string`
    //（本文件所有 `.l()` 读回的字符串都经 `cast_local` + 它转换）。
    Ok(Some(s.try_to_string(env).map_err(JniErr::from)?))
}

/// Java 侧方法名契约（`jni_str!` 要求字面量，常量与字面量由单测钉在一起）。
/// 只被测试引用 —— 生产代码里出现的是字面量（`jni_str!` 强制要求）。
#[allow(dead_code)]
pub(crate) const JAVA_METHOD_SAF_RESULT: &str = "nativeOnSafResult";

/// 注册 `nativeOnSafResult`（`android_main` 期间调，与生命周期桥同位置）。
///
/// 失败只 `warn`：App 照常跑，只是 SAF 不可用（launch 会报缺 Java 块）。
#[cfg(target_os = "android")]
pub fn bind_saf_hooks(app: &crate::android::AndroidApp) {
    static BOUND: AtomicBool = AtomicBool::new(false);
    if BOUND.swap(true, Ordering::AcqRel) {
        return;
    }
    if let Err(e) = bind_inner(app) {
        BOUND.store(false, Ordering::Release);
        log::warn!(
            "vireo: SAF hooks not bound ({e:?}). MainActivity.java must declare \
             `private static native void nativeOnSafResult(long, String, String);` plus the \
             SAF block from examples/android/SAF_SNIPPET.java. Without it the document \
             picker cannot return results."
        );
    }
}

#[cfg(target_os = "android")]
fn bind_inner(app: &crate::android::AndroidApp) -> Result<(), jni::errors::Error> {
    let vm = unsafe { jni::JavaVM::from_raw(app.vm_as_ptr() as *mut jni::sys::JavaVM) };
    vm.attach_current_thread(|env| {
        let raw = app.activity_as_ptr() as jni::sys::jobject;
        if raw.is_null() {
            return Err(jni::errors::Error::NullPtr("activity"));
        }
        let activity = unsafe { jni::objects::JObject::from_raw(env, raw) };
        let class = env.get_object_class(&activity)?;
        // SAFETY: 函数指针是 `extern "system" fn(EnvUnowned, JClass, jlong, JString, JString)`，
        // 与 `static native ... (JLjava/lang/String;Ljava/lang/String;)V` 一致；无捕获无状态。
        let methods = [unsafe {
            jni::NativeMethod::from_raw_parts(
                jni_str!("nativeOnSafResult"),
                jni_str!("(JLjava/lang/String;Ljava/lang/String;)V"),
                vireo_android_on_saf_result as *mut std::ffi::c_void,
            )
        }];
        unsafe { env.register_native_methods(class, &methods) }
    })
}

// ---- JNI 小件 ------------------------------------------------------------------
// 全是直调，无缓存：SAF 操作不在热路径上（选择器/文件开关量级），
// 每次现查最不容易因 Activity 重建 stale。

/// `Intent` 授权 flag（AOSP 稳定值，见 `android.content.Intent`）。
///
/// 注意：`PERSISTABLE` 不在这里 —— launch Intent 由 Java 侧组装，
/// 三个 flag 都在 `MainActivity.vireoLaunchSaf` 里加，Rust 侧只负责 take。
#[cfg(target_os = "android")]
mod flags {
    pub const READ: i32 = 0x00000001;
    pub const WRITE: i32 = 0x00000002;
}

#[cfg(target_os = "android")]
use std::sync::atomic::AtomicBool;

#[cfg(target_os = "android")]
fn parse_uri<'local>(
    env: &mut jni::Env<'local>,
    uri_str: &str,
) -> JRes<jni::objects::JObject<'local>> {
    let cls = env
        .find_class(jni_str!("android/net/Uri"))
        .map_err(JniErr::from)?;
    let s = env.new_string(uri_str).map_err(JniErr::from)?;
    Ok(env
        .call_static_method(
            cls,
            jni_str!("parse"),
            jni_sig!("(Ljava/lang/String;)Landroid/net/Uri;"),
            &[jni::objects::JValue::Object(&s.into())],
        )
        .map_err(JniErr::from)?
        .l()
        .map_err(JniErr::from)?)
}

#[cfg(target_os = "android")]
fn content_resolver<'local>(
    env: &mut jni::Env<'local>,
    activity: &jni::objects::JObject<'_>,
) -> JRes<jni::objects::JObject<'local>> {
    Ok(env
        .call_method(
            activity,
            jni_str!("getContentResolver"),
            jni_sig!("()Landroid/content/ContentResolver;"),
            &[],
        )
        .map_err(JniErr::from)?
        .l()
        .map_err(JniErr::from)?)
}

/// 持久化授权（收到结果时调一次）。失败只 `warn`：URI 本次会话仍可用，降级不报错。
#[cfg(target_os = "android")]
fn persist_best_effort(env: &mut jni::Env, uri_str: &str, kind: SafKind) {
    // take 需要读/写 flag：单文件读只拿了读授权，多要 WRITE 会抛 SecurityException。
    let write = !matches!(kind, SafKind::Open);
    persist_take(env, uri_str, write);
}

/// `takePersistableUriPermission` 本体。失败只 `warn` 不 `Err`
///（见 [`persist_best_effort`]）：抛异常的 provider 照样给本次会话授权。
#[cfg(target_os = "android")]
fn persist_take(env: &mut jni::Env, uri_str: &str, write: bool) {
    use jni::objects::JValue;
    let Some(app) = crate::app::android_app() else {
        return;
    };
    let raw = app.activity_as_ptr() as jni::sys::jobject;
    if raw.is_null() {
        return;
    }
    // SAFETY: 与 `with_activity` 同约定 —— glue 持有的全局引用，借用不删除。
    let activity = unsafe { jni::objects::JObject::from_raw(env, raw) };
    let r: JRes<()> = (|| {
        let resolver = content_resolver(env, &activity)?;
        let uri = parse_uri(env, uri_str)?;
        let mut flag = flags::READ;
        if write {
            flag |= flags::WRITE;
        }
        let r = env.call_method(
            &resolver,
            jni_str!("takePersistableUriPermission"),
            jni_sig!("(Landroid/net/Uri;I)V"),
            &[JValue::Object(&uri), JValue::Int(flag)],
        );
        if let Err(jni::errors::Error::JavaException) = &r {
            let _ = env.exception_clear();
            return Err(JniErr::Unsupported("takePersistableUriPermission"));
        }
        r.map(|_| ()).map_err(JniErr::from)?;
        Ok(())
    })();
    if r.is_err() {
        log::warn!(
            "vireo: SAF persistable permission not taken for {uri_str:?} \
             (provider may not support it); the grant is valid for this session only"
        );
    }
}

#[cfg(target_os = "android")]
fn open_fd_impl(saf: &SafUri, mode: &str) -> Result<std::fs::File, VireoError> {
    let app = crate::app::android_app().ok_or_else(|| {
        VireoError::Other(
            "AndroidApp unavailable: SAF needs the #[vireo::main] android_main entry".to_string(),
        )
    })?;
    with_activity(app, |env, activity| {
        let resolver = content_resolver(env, activity)?;
        let uri = parse_uri(env, &saf.uri)?;
        let mode = env.new_string(mode).map_err(JniErr::from)?;
        let pfd = env
            .call_method(
                &resolver,
                jni_str!("openFileDescriptor"),
                jni_sig!("(Landroid/net/Uri;Ljava/lang/String;)Landroid/os/ParcelFileDescriptor;"),
                &[
                    jni::objects::JValue::Object(&uri),
                    jni::objects::JValue::Object(&mode.into()),
                ],
            )
            .map_err(JniErr::from)?
            .l()
            .map_err(JniErr::from)?;
        if pfd.as_raw().is_null() {
            return Err(JniErr::Null("ParcelFileDescriptor"));
        }
        let fd = env
            .call_method(&pfd, jni_str!("detachFd"), jni_sig!("()I"), &[])
            .map_err(JniErr::from)?
            .i()
            .map_err(JniErr::from)?;
        if fd < 0 {
            return Err(JniErr::BadValue("detachFd", fd as f64));
        }
        // SAFETY: detachFd 交出所有权；fd 有效（刚判过）；File 接管后负责 close。
        // CLOEXEC：ParcelFileDescriptor 的 fd 本来就是 CLOEXEC 建的，游戏进程也不 exec。
        Ok(unsafe { std::fs::File::from_raw_fd(fd) })
    })
    .map_err(|e| VireoError::Other(format!("SAF open failed for {:?}: {e:?}", saf.uri)))
}

#[cfg(target_os = "android")]
fn docs_class<'local>(env: &mut jni::Env<'local>) -> JRes<jni::objects::JClass<'local>> {
    env.find_class(jni_str!("android/provider/DocumentsContract"))
        .map_err(JniErr::from)
}

#[cfg(target_os = "android")]
fn doc_class<'local>(env: &mut jni::Env<'local>) -> JRes<jni::objects::JClass<'local>> {
    env.find_class(jni_str!("android/provider/DocumentsContract$Document"))
        .map_err(JniErr::from)
}

#[cfg(target_os = "android")]
fn doc_str_field<'local>(env: &mut jni::Env<'local>, field: &jni::strings::JNIStr) -> JRes<String> {
    let cls = doc_class(env)?;
    let v = env
        .get_static_field(cls, field, jni_sig!("Ljava/lang/String;"))
        .map_err(JniErr::from)?
        .l()
        .map_err(JniErr::from)?;
    let s = env
        .cast_local::<jni::objects::JString>(v)
        .map_err(JniErr::from)?;
    s.try_to_string(env).map_err(JniErr::from)
}

#[cfg(target_os = "android")]
fn tree_doc_id(env: &mut jni::Env, tree_uri: &jni::objects::JObject) -> JRes<String> {
    let cls = docs_class(env)?;
    let id = env
        .call_static_method(
            cls,
            jni_str!("getTreeDocumentId"),
            jni_sig!("(Landroid/net/Uri;)Ljava/lang/String;"),
            &[jni::objects::JValue::Object(tree_uri)],
        )
        .map_err(JniErr::from)?
        .l()
        .map_err(JniErr::from)?;
    let s = env
        .cast_local::<jni::objects::JString>(id)
        .map_err(JniErr::from)?;
    s.try_to_string(env).map_err(JniErr::from)
}

#[cfg(target_os = "android")]
fn build_doc_uri<'local>(
    env: &mut jni::Env<'local>,
    tree_uri: &jni::objects::JObject,
    doc_id: &str,
) -> JRes<jni::objects::JObject<'local>> {
    let cls = docs_class(env)?;
    let id = env.new_string(doc_id).map_err(JniErr::from)?;
    Ok(env
        .call_static_method(
            cls,
            jni_str!("buildDocumentUriUsingTree"),
            jni_sig!("(Landroid/net/Uri;Ljava/lang/String;)Landroid/net/Uri;"),
            &[
                jni::objects::JValue::Object(tree_uri),
                jni::objects::JValue::Object(&id.into()),
            ],
        )
        .map_err(JniErr::from)?
        .l()
        .map_err(JniErr::from)?)
}

#[cfg(target_os = "android")]
fn build_children_uri<'local>(
    env: &mut jni::Env<'local>,
    tree_uri: &jni::objects::JObject,
    parent_id: &str,
) -> JRes<jni::objects::JObject<'local>> {
    let cls = docs_class(env)?;
    let id = env.new_string(parent_id).map_err(JniErr::from)?;
    Ok(env
        .call_static_method(
            cls,
            jni_str!("buildChildDocumentsUriUsingTree"),
            jni_sig!("(Landroid/net/Uri;Ljava/lang/String;)Landroid/net/Uri;"),
            &[
                jni::objects::JValue::Object(tree_uri),
                jni::objects::JValue::Object(&id.into()),
            ],
        )
        .map_err(JniErr::from)?
        .l()
        .map_err(JniErr::from)?)
}

/// 子行查询：`query` 不支持 selection（部分 provider 直接忽略/抛错），
/// 所以全列出来 Rust 侧按显示名匹配 —— 目录条目数小，这么做最稳。
#[cfg(target_os = "android")]
fn query_children(
    env: &mut jni::Env,
    resolver: &jni::objects::JObject,
    children_uri: &jni::objects::JObject,
) -> JRes<Vec<(String, String, String)>> {
    use jni::objects::{JObject, JValue};
    let col_id = doc_str_field(env, jni_str!("COLUMN_DOCUMENT_ID"))?;
    let col_name = doc_str_field(env, jni_str!("COLUMN_DISPLAY_NAME"))?;
    let col_mime = doc_str_field(env, jni_str!("COLUMN_MIME_TYPE"))?;
    let proj = env
        .new_object_array(3, jni_str!("java/lang/String"), JObject::null())
        .map_err(JniErr::from)?;
    for (i, c) in [&col_id, &col_name, &col_mime].iter().enumerate() {
        let js = env.new_string(c).map_err(JniErr::from)?;
        proj.set_element(env, i, &js).map_err(JniErr::from)?;
        env.delete_local_ref(js);
    }
    let null = JObject::null();
    let cursor = env
        .call_method(
            resolver,
            jni_str!("query"),
            jni_sig!("(Landroid/net/Uri;[Ljava/lang/String;Ljava/lang/String;[Ljava/lang/String;Ljava/lang/String;)Landroid/database/Cursor;"),
            &[
                JValue::Object(children_uri),
                JValue::Object(&proj.into()),
                JValue::Object(&null),
                JValue::Object(&null),
                JValue::Object(&null),
            ],
        )
        .map_err(JniErr::from)?
        .l()
        .map_err(JniErr::from)?;
    if cursor.as_raw().is_null() {
        return Err(JniErr::Null("Cursor"));
    }
    // 行循环包进 local frame：条目再多也不会撑爆 local ref 表；
    // 取出的 Rust String 在 frame 弹出后依然有效。
    let out = env
        .with_local_frame(32, |env| -> JRes<Vec<(String, String, String)>> {
            let mut out = Vec::new();
            loop {
                let more = env
                    .call_method(&cursor, jni_str!("moveToNext"), jni_sig!("()Z"), &[])
                    .map_err(JniErr::from)?
                    .z()
                    .map_err(JniErr::from)?;
                if !more {
                    break;
                }
                let mut col = |name: &str| -> JRes<String> {
                    let js = env.new_string(name).map_err(JniErr::from)?;
                    let idx = env
                        .call_method(
                            &cursor,
                            jni_str!("getColumnIndex"),
                            jni_sig!("(Ljava/lang/String;)I"),
                            &[JValue::Object(&js.into())],
                        )
                        .map_err(JniErr::from)?
                        .i()
                        .map_err(JniErr::from)?;
                    if idx < 0 {
                        return Err(JniErr::Unsupported("missing column"));
                    }
                    let v = env
                        .call_method(
                            &cursor,
                            jni_str!("getString"),
                            jni_sig!("(I)Ljava/lang/String;"),
                            &[JValue::Int(idx)],
                        )
                        .map_err(JniErr::from)?
                        .l()
                        .map_err(JniErr::from)?;
                    if v.as_raw().is_null() {
                        return Err(JniErr::Null("column value"));
                    }
                    let s = env
                        .cast_local::<jni::objects::JString>(v)
                        .map_err(JniErr::from)?;
                    s.try_to_string(env).map_err(JniErr::from)
                };
                out.push((col(&col_id)?, col(&col_name)?, col(&col_mime)?));
            }
            Ok(out)
        })
        .map_err(JniErr::from)?;
    let _ = env.call_method(&cursor, jni_str!("close"), jni_sig!("()V"), &[]);
    let _ = env.exception_clear();
    Ok(out)
}

#[cfg(target_os = "android")]
const MIME_TYPE_DIR: &str = "vnd.android.document/directory";

/// 沿目录链走到父级，返回父级的 document id（`""` = 树根）。
#[cfg(target_os = "android")]
fn walk_to_parent_id(
    env: &mut jni::Env,
    resolver: &jni::objects::JObject,
    tree_uri: &jni::objects::JObject,
    segs: &[String],
    expect_dirs_through: usize,
) -> JRes<String> {
    let mut parent = tree_doc_id(env, tree_uri)?;
    for seg in &segs[..expect_dirs_through] {
        parent = find_child_id(env, resolver, tree_uri, &parent, seg, true)?.0;
    }
    Ok(parent)
}

#[cfg(target_os = "android")]
fn find_child_id(
    env: &mut jni::Env,
    resolver: &jni::objects::JObject,
    tree_uri: &jni::objects::JObject,
    parent_id: &str,
    name: &str,
    expect_dir: bool,
) -> JRes<(String, bool)> {
    let children = build_children_uri(env, tree_uri, parent_id)?;
    let rows = query_children(env, resolver, &children)?;
    for (doc_id, display, mime) in rows {
        if display == name {
            let is_dir = mime == MIME_TYPE_DIR;
            if expect_dir && !is_dir {
                return Err(JniErr::Unsupported("not a directory"));
            }
            return Ok((doc_id, is_dir));
        }
    }
    Err(JniErr::Null("child"))
}

#[cfg(target_os = "android")]
fn tree_parts(saf: &SafUri) -> Result<(), VireoError> {
    if !saf.is_tree {
        return Err(VireoError::Other(format!(
            "not a directory URI: {:?} (tree operations need an open_document_tree URI)",
            saf.uri
        )));
    }
    Ok(())
}

#[cfg(target_os = "android")]
fn with_tree<F, R>(saf: &SafUri, f: F) -> Result<R, VireoError>
where
    F: FnOnce(&mut jni::Env, &jni::objects::JObject, &jni::objects::JObject) -> Result<R, JniErr>,
{
    tree_parts(saf)?;
    let app = crate::app::android_app().ok_or_else(|| {
        VireoError::Other(
            "AndroidApp unavailable: SAF needs the #[vireo::main] android_main entry".to_string(),
        )
    })?;
    with_activity(app, |env, activity| {
        let resolver = content_resolver(env, activity)?;
        let tree = parse_uri(env, &saf.uri)?;
        f(env, &resolver, &tree)
    })
    .map_err(|e| VireoError::Other(format!("SAF failed for {:?}: {e:?}", saf.uri)))
}

#[cfg(target_os = "android")]
fn list_impl(saf: &SafUri, rel: &str) -> Result<Vec<SafEntry>, VireoError> {
    let segs =
        normalize_rel(rel).ok_or_else(|| VireoError::Other(format!("invalid path: {rel:?}")))?;
    with_tree(saf, |env, resolver, tree| {
        let n = segs.len();
        let parent = walk_to_parent_id(env, resolver, tree, &segs, n)?;
        let children = build_children_uri(env, tree, &parent)?;
        let rows = query_children(env, resolver, &children)?;
        Ok(rows
            .into_iter()
            .map(|(_, name, mime)| SafEntry {
                name,
                is_dir: mime == MIME_TYPE_DIR,
            })
            .collect())
    })
}

#[cfg(target_os = "android")]
fn resolve_impl(saf: &SafUri, rel: &str) -> Result<SafUri, VireoError> {
    let segs =
        normalize_rel(rel).ok_or_else(|| VireoError::Other(format!("invalid path: {rel:?}")))?;
    if segs.is_empty() {
        return Ok(saf.clone());
    }
    with_tree(saf, |env, resolver, tree| {
        let n = segs.len();
        let parent = walk_to_parent_id(env, resolver, tree, &segs, n - 1)?;
        let (doc_id, _) = find_child_id(env, resolver, tree, &parent, &segs[n - 1], false)?;
        let doc = build_doc_uri(env, tree, &doc_id)?;
        let uri_obj = env
            .call_method(
                &doc,
                jni_str!("toString"),
                jni_sig!("()Ljava/lang/String;"),
                &[],
            )
            .map_err(JniErr::from)?
            .l()
            .map_err(JniErr::from)?;
        let s = env
            .cast_local::<jni::objects::JString>(uri_obj)
            .map_err(JniErr::from)?;
        let uri = s.try_to_string(env).map_err(JniErr::from)?;
        Ok(SafUri {
            uri,
            is_tree: false,
        })
    })
}

#[cfg(target_os = "android")]
fn create_impl(saf: &SafUri, rel_dir: &str, mime: &str, name: &str) -> Result<SafUri, VireoError> {
    let segs = normalize_rel(rel_dir)
        .ok_or_else(|| VireoError::Other(format!("invalid path: {rel_dir:?}")))?;
    if name.contains('/') || name.contains('\\') || name.is_empty() {
        return Err(VireoError::Other(format!(
            "invalid document name: {name:?}"
        )));
    }
    with_tree(saf, |env, resolver, tree| {
        let n = segs.len();
        let parent = walk_to_parent_id(env, resolver, tree, &segs, n)?;
        let parent_uri = build_doc_uri(env, tree, &parent)?;
        let cls = docs_class(env)?;
        let mime_js = env.new_string(mime).map_err(JniErr::from)?;
        let name_js = env.new_string(name).map_err(JniErr::from)?;
        let created = env
            .call_static_method(
                cls,
                jni_str!("createDocument"),
                jni_sig!("(Landroid/content/ContentResolver;Landroid/net/Uri;Ljava/lang/String;Ljava/lang/String;)Landroid/net/Uri;"),
                &[
                    jni::objects::JValue::Object(resolver),
                    jni::objects::JValue::Object(&parent_uri),
                    jni::objects::JValue::Object(&mime_js.into()),
                    jni::objects::JValue::Object(&name_js.into()),
                ],
            )
            .map_err(JniErr::from)?
            .l()
            .map_err(JniErr::from)?;
        if created.as_raw().is_null() {
            return Err(JniErr::Null("created document"));
        }
        let uri_obj = env
            .call_method(
                &created,
                jni_str!("toString"),
                jni_sig!("()Ljava/lang/String;"),
                &[],
            )
            .map_err(JniErr::from)?
            .l()
            .map_err(JniErr::from)?;
        let s = env
            .cast_local::<jni::objects::JString>(uri_obj)
            .map_err(JniErr::from)?;
        let uri = s.try_to_string(env).map_err(JniErr::from)?;
        Ok(SafUri {
            uri,
            is_tree: false,
        })
    })
}

#[cfg(target_os = "android")]
fn delete_impl(saf: &SafUri, rel: &str) -> Result<(), VireoError> {
    let target = resolve_impl(saf, rel)?;
    with_tree(saf, |env, resolver, _| {
        let cls = docs_class(env)?;
        let uri = parse_uri(env, &target.uri)?;
        let ok = env
            .call_static_method(
                cls,
                jni_str!("deleteDocument"),
                jni_sig!("(Landroid/content/ContentResolver;Landroid/net/Uri;)Z"),
                &[
                    jni::objects::JValue::Object(resolver),
                    jni::objects::JValue::Object(&uri),
                ],
            )
            .map_err(JniErr::from)?
            .z()
            .map_err(JniErr::from)?;
        if !ok {
            return Err(JniErr::Unsupported("delete rejected"));
        }
        Ok(())
    })
}

/// 从 Cursor 读一列字符串（`getColumnIndex` + `getString`，可空）。
#[cfg(target_os = "android")]
fn read_col_str<'local>(
    env: &mut jni::Env<'local>,
    cursor: &jni::objects::JObject,
    col: &str,
) -> JRes<jni::objects::JObject<'local>> {
    use jni::objects::JValue;
    let js = env.new_string(col).map_err(JniErr::from)?;
    let idx = env
        .call_method(
            cursor,
            jni_str!("getColumnIndex"),
            jni_sig!("(Ljava/lang/String;)I"),
            &[JValue::Object(&js.into())],
        )
        .map_err(JniErr::from)?
        .i()
        .map_err(JniErr::from)?;
    if idx < 0 {
        return Err(JniErr::Unsupported("missing column"));
    }
    env.call_method(
        cursor,
        jni_str!("getString"),
        jni_sig!("(I)Ljava/lang/String;"),
        &[JValue::Int(idx)],
    )
    .map_err(JniErr::from)?
    .l()
    .map_err(JniErr::from)
}

#[cfg(target_os = "android")]
fn meta_impl(saf: &SafUri) -> Result<(String, Option<u64>), VireoError> {
    use jni::objects::{JObject, JValue};
    let app = crate::app::android_app().ok_or_else(|| {
        VireoError::Other(
            "AndroidApp unavailable: SAF needs the #[vireo::main] android_main entry".to_string(),
        )
    })?;
    with_activity(app, |env, activity| {
        let resolver = content_resolver(env, activity)?;
        let uri = parse_uri(env, &saf.uri)?;
        let col_name = doc_str_field(env, jni_str!("COLUMN_DISPLAY_NAME"))?;
        let col_size = doc_str_field(env, jni_str!("COLUMN_SIZE"))?;
        let proj = env
            .new_object_array(2, jni_str!("java/lang/String"), JObject::null())
            .map_err(JniErr::from)?;
        for (i, c) in [&col_name, &col_size].iter().enumerate() {
            let js = env.new_string(c).map_err(JniErr::from)?;
            proj.set_element(env, i, &js).map_err(JniErr::from)?;
            env.delete_local_ref(js);
        }
        let null = JObject::null();
        let cursor = env
            .call_method(
                &resolver,
                jni_str!("query"),
                jni_sig!("(Landroid/net/Uri;[Ljava/lang/String;Ljava/lang/String;[Ljava/lang/String;Ljava/lang/String;)Landroid/database/Cursor;"),
                &[
                    JValue::Object(&uri),
                    JValue::Object(&proj.into()),
                    JValue::Object(&null),
                    JValue::Object(&null),
                    JValue::Object(&null),
                ],
            )
            .map_err(JniErr::from)?
            .l()
            .map_err(JniErr::from)?;
        if cursor.as_raw().is_null() {
            return Err(JniErr::Null("Cursor"));
        }
        let first = env
            .call_method(&cursor, jni_str!("moveToFirst"), jni_sig!("()Z"), &[])
            .map_err(JniErr::from)?
            .z()
            .map_err(JniErr::from)?;
        if !first {
            let _ = env.call_method(&cursor, jni_str!("close"), jni_sig!("()V"), &[]);
            let _ = env.exception_clear();
            return Err(JniErr::Null("empty metadata"));
        }
        let name_js = read_col_str(env, &cursor, &col_name)?;
        let name: String = if name_js.as_raw().is_null() {
            String::new()
        } else {
            let s = env
                .cast_local::<jni::objects::JString>(name_js)
                .map_err(JniErr::from)?;
            s.try_to_string(env).map_err(JniErr::from)?
        };
        let size_js = read_col_str(env, &cursor, &col_size)?;
        let size: Option<u64> = if size_js.as_raw().is_null() {
            None
        } else {
            let s = env
                .cast_local::<jni::objects::JString>(size_js)
                .map_err(JniErr::from)?;
            let raw = s.try_to_string(env).map_err(JniErr::from)?;
            raw.parse::<i64>().ok().filter(|v| *v >= 0).map(|v| v as u64)
        };
        let _ = env.call_method(&cursor, jni_str!("close"), jni_sig!("()V"), &[]);
        let _ = env.exception_clear();
        Ok((name, size))
    })
    .map_err(|e| VireoError::Other(format!("SAF metadata failed for {:?}: {e:?}", saf.uri)))
}

#[cfg(target_os = "android")]
fn release_impl(uri_str: &str) -> Result<(), VireoError> {
    use jni::objects::JValue;
    let app = crate::app::android_app().ok_or_else(|| {
        VireoError::Other(
            "AndroidApp unavailable: SAF needs the #[vireo::main] android_main entry".to_string(),
        )
    })?;
    with_activity(app, |env, activity| {
        let resolver = content_resolver(env, activity)?;
        let uri = parse_uri(env, uri_str)?;
        // 读写分开释：只持有读授权时合在一起调会抛 SecurityException，
        // 那样读授权也释不掉。
        for flag in [flags::WRITE, flags::READ] {
            let r = env.call_method(
                &resolver,
                jni_str!("releasePersistableUriPermission"),
                jni_sig!("(Landroid/net/Uri;I)V"),
                &[JValue::Object(&uri), JValue::Int(flag)],
            );
            if let Err(jni::errors::Error::JavaException) = &r {
                let _ = env.exception_clear();
            } else {
                r.map(|_| ()).map_err(JniErr::from)?;
            }
        }
        Ok(())
    })
    .map_err(|e| VireoError::Other(format!("SAF release failed for {uri_str:?}: {e:?}")))
}

#[cfg(target_os = "android")]
fn persisted_impl() -> Result<Vec<SafGrant>, VireoError> {
    use jni::objects::JValue;
    let app = crate::app::android_app().ok_or_else(|| {
        VireoError::Other(
            "AndroidApp unavailable: SAF needs the #[vireo::main] android_main entry".to_string(),
        )
    })?;
    with_activity(app, |env, activity| {
        let resolver = content_resolver(env, activity)?;
        let list = env
            .call_method(
                &resolver,
                jni_str!("getPersistedUriPermissions"),
                jni_sig!("()Ljava/util/List;"),
                &[],
            )
            .map_err(JniErr::from)?
            .l()
            .map_err(JniErr::from)?;
        let n = env
            .call_method(&list, jni_str!("size"), jni_sig!("()I"), &[])
            .map_err(JniErr::from)?
            .i()
            .map_err(JniErr::from)?;
        let mut out = Vec::new();
        for i in 0..n {
            let item = env
                .call_method(
                    &list,
                    jni_str!("get"),
                    jni_sig!("(I)Ljava/lang/Object;"),
                    &[JValue::Int(i)],
                )
                .map_err(JniErr::from)?
                .l()
                .map_err(JniErr::from)?;
            let uri_obj = env
                .call_method(
                    &item,
                    jni_str!("getUri"),
                    jni_sig!("()Landroid/net/Uri;"),
                    &[],
                )
                .map_err(JniErr::from)?
                .l()
                .map_err(JniErr::from)?;
            let uri_str = env
                .call_method(
                    &uri_obj,
                    jni_str!("toString"),
                    jni_sig!("()Ljava/lang/String;"),
                    &[],
                )
                .map_err(JniErr::from)?
                .l()
                .map_err(JniErr::from)?;
            let s = env
                .cast_local::<jni::objects::JString>(uri_str)
                .map_err(JniErr::from)?;
            let uri = s.try_to_string(env).map_err(JniErr::from)?;
            let readable = env
                .call_method(&item, jni_str!("isReadPermission"), jni_sig!("()Z"), &[])
                .map_err(JniErr::from)?
                .z()
                .map_err(JniErr::from)?;
            let writable = env
                .call_method(&item, jni_str!("isWritePermission"), jni_sig!("()Z"), &[])
                .map_err(JniErr::from)?
                .z()
                .map_err(JniErr::from)?;
            env.delete_local_ref(item);
            env.delete_local_ref(uri_obj);
            out.push(SafGrant {
                uri: SafUri::from_uri_string(uri),
                readable,
                writable,
            });
        }
        Ok(out)
    })
    .map_err(|e| VireoError::Other(format!("SAF persisted list failed: {e:?}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_rel_accepts_root_and_segments() {
        assert_eq!(normalize_rel(""), Some(vec![]));
        assert_eq!(
            normalize_rel("levels/01/chart.json"),
            Some(vec![
                "levels".to_string(),
                "01".to_string(),
                "chart.json".to_string()
            ])
        );
        // 分隔符两种都行，前导 / 与 ./ 照单全收
        assert_eq!(
            normalize_rel("/a\\b/./c"),
            Some(vec!["a".to_string(), "b".to_string(), "c".to_string()])
        );
    }

    #[test]
    fn normalize_rel_rejects_parent_traversal() {
        assert_eq!(normalize_rel(".."), None);
        assert_eq!(normalize_rel("a/../../b"), None);
        assert_eq!(normalize_rel("a\\..\\b"), None);
    }

    #[test]
    fn from_uri_string_detects_tree_by_convention() {
        let t = SafUri::from_uri_string(
            "content://com.android.externalstorage.documents/tree/primary%3AMusic",
        );
        assert!(t.is_tree());
        assert_eq!(
            t.uri(),
            "content://com.android.externalstorage.documents/tree/primary%3AMusic"
        );
        let d = SafUri::from_uri_string(
            "content://com.android.externalstorage.documents/document/primary%3AMusic%2Fa.json",
        );
        assert!(!d.is_tree());
    }

    #[test]
    fn desktop_stubs_are_unsupported_not_panics() {
        // 桌面（单测环境）所有 JNI 入口都是 Unsupported，不是 panic。
        let u = SafUri::from_uri_string("content://example/tree/1");
        assert!(matches!(u.open_reader(), Err(VireoError::Unsupported(_))));
        assert!(matches!(u.open_writer(), Err(VireoError::Unsupported(_))));
        assert!(matches!(u.display_name(), Err(VireoError::Unsupported(_))));
        assert!(matches!(u.size(), Err(VireoError::Unsupported(_))));
        assert!(matches!(u.list(""), Err(VireoError::Unsupported(_))));
        assert!(matches!(u.resolve("a"), Err(VireoError::Unsupported(_))));
        assert!(matches!(
            u.create_file("", "text/plain", "a.txt"),
            Err(VireoError::Unsupported(_))
        ));
        assert!(matches!(
            u.create_dir("", "d"),
            Err(VireoError::Unsupported(_))
        ));
        assert!(matches!(u.delete("a"), Err(VireoError::Unsupported(_))));
        assert!(matches!(
            u.take_persistable(true),
            Err(VireoError::Unsupported(_))
        ));
        assert!(matches!(u.release(), Err(VireoError::Unsupported(_))));
        assert!(matches!(
            SafUri::persisted(),
            Err(VireoError::Unsupported(_))
        ));
        let r = request(SafKind::Tree, "act", None, &[], None, Box::new(|_| {}));
        assert!(matches!(r, Err(VireoError::Unsupported(_))));
        assert!(!cancel_pending());
        assert!(drain_results().is_empty());
    }

    #[test]
    fn saf_state_single_flight() {
        // 状态机本身是纯逻辑：第二个在途请求必须被拒，不能静默串回调。
        reset_for_tests();
        let mut st = SafState {
            pending: None,
            results: VecDeque::new(),
        };
        assert!(st.pending.is_none());
        st.pending = Some(Pending {
            id: 7,
            kind: SafKind::Tree,
            cb: Box::new(|_| {}),
        });
        // take_if 只吃对上 id 的
        assert!(st.pending.take_if(|p| p.id == 8).is_none());
        assert!(st.pending.take_if(|p| p.id == 7).is_some());
        assert!(st.pending.is_none());
        reset_for_tests();
    }

    #[test]
    fn java_method_name_is_stable() {
        // Java↔Rust 契约：改名会让已发布的 MainActivity 模板静默失效
        //（绑定失败只 warn）。`jni_str!` 要求字面量，此常量只用于文档与测试。
        assert_eq!(super::JAVA_METHOD_SAF_RESULT, "nativeOnSafResult");
    }

    #[test]
    fn error_display_is_english() {
        // 诊断输出统一英文（含新变体）。
        assert_eq!(
            VireoError::UserCancelled.to_string(),
            "user cancelled the picker"
        );
        assert_eq!(
            VireoError::Unsupported("SAF document picker").to_string(),
            "SAF document picker is only supported on Android"
        );
    }
}
