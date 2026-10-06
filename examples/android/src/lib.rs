//! vireo Android 模板入口。
//!
//! 这是一个 **cdylib**（`crate-type = ["cdylib"]`，入口在 `src/lib.rs` 而非
//! `src/main.rs`）—— Android 侧由 AGSK 通过 `#[no_mangle] android_main` 调用，
//! 不需要 Rust 的 `fn main`。
//!
//! Android API 全部 `cfg` 门控，所以桌面上 `cargo check` 也能跑，
//! 用来在没装 Android 工具链时先把代码笔误抓出来：
//!
//! ```bash
//! cargo check                          # 桌面，只查非 Android 分支
//! cargo ndk -t arm64-v8a check --lib   # Android，查全部
//! ```
//!
//! 想看**能跑**的桌面示例，去仓库根的 `examples/`（`cargo run --example ez`）；
//! 本目录专注 Android 构建链。
//!
//! 演示三件事：
//! 1. **沉浸式 + 安全区三值**：`safe_area` / `padding` / `ime_insets` 各画一圈，
//!    直接看出刘海、状态栏、键盘各占多少。
//! 2. **文本输入**：点输入框 → `show_soft_input()` → 每帧读 `text_input_state()` 画出来。
//!    中文可用（GameActivity 提供 `InputConnection`）。
//! 3. **键盘避让**：键盘弹出时把输入框上移 `ime_insets.bottom`。

// 这个 cdylib 的**唯一**入口是 `#[vireo::main]` 展开出的 `android_main`，AGDK
// 在运行时按符号名查找它。rustc 的 dead_code 分析看不到「JNI 按字符串找符号」
// 这层间接引用，于是把 android_main 里用到的私有项全报成「未使用」。
//
// 这不是笔误 —— 真机上代码确实在跑。对纯 JNI cdylib 来说整个 crate 都是
// 「只被 JNI 到达」，所以这里整 crate 放宽。
#![allow(dead_code)]

use std::sync::atomic::Ordering;

use vireo::prelude::*;

/// 安全区四边（逻辑像素）。
///
/// vireo 的 `platform::android::Insets` 只在 Android 上编译，所以用本地结构体
/// 让同一份绘制代码在桌面也能过 `cargo check`。
#[derive(Clone, Copy, Default)]
struct Edges {
    l: f32,
    t: f32,
    r: f32,
    b: f32,
}

#[cfg(target_os = "android")]
impl From<vireo::platform::android::Insets> for Edges {
    fn from(i: vireo::platform::android::Insets) -> Self {
        Self {
            l: i.left,
            t: i.top,
            r: i.right,
            b: i.bottom,
        }
    }
}

/// 生命周期 / 闸门观测计数器（HUD 显示，真机一眼看出各机制是否生效）。
///
/// 字段是 `Arc` —— 回调要 `'static`，必须 clone 一份带进闭包，而**计数必须共享**。
///
/// 踩过的坑：最初这里是裸 `AtomicU32` + 手写 `Clone`（逐字段复制），结果回调自增的是
/// **副本**，HUD 读原处恒为 0，看起来像「回调从没触发」。真机验证时只有 logcat 可信，
/// HUD 计数一度把我引向错误结论。
#[derive(Clone, Default)]
struct LifeCounters {
    paused: std::sync::Arc<std::sync::atomic::AtomicU32>,
    stopped: std::sync::Arc<std::sync::atomic::AtomicU32>,
    destroyed: std::sync::Arc<std::sync::atomic::AtomicU32>,
    /// `auto_pause=false` 时后台仍执行的帧数（证明闸门真的关掉了）。
    bg_frames: std::sync::Arc<std::sync::atomic::AtomicU32>,
    /// `wait_while_paused()` 返回后累计次数（证明「自己决定何时停」这条路通）。
    waited: std::sync::Arc<std::sync::atomic::AtomicU32>,
    /// `wait_until` + `notify_waiters` 往返成功次数（自定义条件等待）。
    wait_until_ok: std::sync::Arc<std::sync::atomic::AtomicU32>,
    /// `auto_pause` 实际处于开启（= vireo 自动阻塞）状态的帧数。
    auto_frames: std::sync::Arc<std::sync::atomic::AtomicU32>,
}

// `fonts = ...` 在 `App` 构造**之前**求值，这是宏路径下唯一能定制 Android 系统字体
// 白名单的时机（`App::new()` 内部就建好了 TextContext 并跑完字体 install()，而本函数体
// 是在那之后才执行的）。
//
// 这里演示三件事：
//   1. `extra_prefixes` —— 内置白名单外的前缀。`SourceSansPro` 是本机
//      `/system/fonts` 里真实存在的正文字体，且**不**被任何内置前缀命中
//      （内置只有 `SourceHanSans`，两者不是同一个字体），所以加了它就能看出
//      files/faces 计数变化。
//   2. `extra_families` —— 必须显式钉家族名。只加前缀不改默认族的话，generic 仍指向
//      fontdb 默认的 "Arial"，Android 上没有字体声明它 → query 返回 None → 回退到
//      「排序后第一个 face」，可能挑中装饰体（症状：全大写 + 数字消失）。
//   3. 排在内置 `PREFERRED_FAMILIES` **之前** —— Roboto 在本机存在，不排在前面
//      这条规则就永远轮不到。
//
// HUD 底行的 `fonts` 行会变成 `default=Source Sans Pro`，据此确认配置生效。
// 真机上把 `CarroisGothicSC` 换进来能看出更明显的字形变化。
#[vireo::main(fonts = |c| {
    c.extra_prefixes.push("SourceSansPro".to_owned());
    c.extra_families.push("Source Sans Pro".to_owned());
})]
async fn main(app: App) {
    // `init_logger` 走 env_logger，**默认 level 是 error** —— 而 vireo 自己的诊断
    // （Android 字体扫描结果、JNI 绑定失败警告）都在 info/warn 级，不设的话
    // logcat 里什么都看不到，只能靠屏幕上的 HUD。这里显式打开 info，
    // 排查字体 / 生命周期问题时 `adb logcat -s vireoandroid` 就够用。
    //
    // ⚠️ 必须**在** `init_logger()` 之前设：env_logger 在初始化时读一次 RUST_LOG。
    // 正式项目里建议改成读 `BuildConfig.DEBUG` 或自己的开关，别把 info 留在 release。
    std::env::set_var("RUST_LOG", "info");

    // 不调这个的话 vireo 的 `log::warn!`（字体加载、IME、生命周期）全被丢弃。
    vireo::init_logger();

    let idx = app.window(WindowDesc::new("Vireo Android", 1280, 720), None::<fn()>);

    // ---- 生命周期：on_paused vs on_stopped ---------------------------------
    // 两者都在**渲染线程**上触发，但时机不同：
    //   on_paused  —— 暂停生效的那一刻（进暂停闸门阻塞之前）
    //   on_stopped —— **恢复时**。「这次暂停算不算彻底离开」是事后才知道的
    //                 （Android 的 onStart 紧跟 onResume，都在渲染线程被唤醒前跑完）
    //
    // 实机验证：adb shell input keyevent 3（HOME）回桌面，再启动回来，
    // 然后 `adb logcat -d | findstr lifecycle` —— 应看到 paused 与 stopped 各一条，
    // 时间戳相差约等于你按 HOME 到回来的间隔。HUD 的 `life` 行也能看计数。
    let life = LifeCounters::default();
    {
        let life = life.clone();
        // 花括号必需：`fetch_add` 返回旧值（u32），`move || expr` 会让闭包返回类型
        // 变成 u32，不满足回调要求的 `Fn()`。
        app.on_paused(move || {
            life.paused.fetch_add(1, Ordering::Relaxed);
            log::info!("vireo-android lifecycle: paused（暂停生效时触发）");
        });
    }
    {
        let life = life.clone();
        app.on_stopped(move || {
            life.stopped.fetch_add(1, Ordering::Relaxed);
            log::info!("vireo-android lifecycle: stopped = 彻底离开（恢复时消费锁存）");
        });
    }
    {
        let life = life.clone();
        app.on_destroyed(move || {
            life.destroyed.fetch_add(1, Ordering::Relaxed);
            log::info!("vireo-android lifecycle: destroyed = 用户主动关闭（划掉最近任务）");
        });
    }

    // ---- 接管暂停时机：默认关掉 vireo 的闸门（A 键可切回对比）----------------
    // 默认（auto_pause = true）时 vireo 自己在切后台时阻塞整条渲染线程。
    // 关掉之后循环照常跑，后台要干什么由我们自己决定 —— 下面 `run` 里
    // 「后台仍 tick 但不 draw」就是这套用法。
    //
    // 启动时设为 false（演示接管），**A 键可随时切回 true 验证默认路径**：
    // 切到 true 后按 HOME，bg_frames 应立刻停止增长 —— 那条分支同样需要回归验证，
    // 不能只靠「auto_pause()=true 时条件退化成 is_paused()」这种推理。
    app.set_auto_pause(false);

    // 自定义等待条件：`wait_until` + `notify_waiters` 的往返验证。
    // 目标条件由「外部线程」满足 —— 这里用一个 3 秒后自己置位的 std 线程，
    // 模拟「等网络响应 / 等某个异步任务完成」这类真实场景。
    let wait_goal = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let goal = wait_goal.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(3));
            goal.store(true, Ordering::Release);
        });
    }
    // ---- 返回键：弹「退出游戏？」二段确认 --------------------------------------
    //
    // 不注册 `on_back` 也能跑，但返回键 = `Activity.finish()` 立即销毁进程 ——
    // 没确认、没存档。所以这里演示标准接法：**第一次按返回键拦住并弹确认，
    // 第二次才真退**。
    //
    // ⚠️ 语义方向容易搞反：**不消费 = 退出**（Android 默认行为）。所以
    // 「拦下来」那一支必须调 `consume_back()`，而「确认退出」那一支要**不调**。
    //
    // ⚠️ 另一条关键规则：`consume_back()` 必须在 `on_back` 回调内**同步**调用。
    // Java 侧在同一轮 `onBackPressed` 里就会读「是否已消费」来决定 finish 执不
    // 执行，所以异步（spawn 到别的线程再调）会来不及 —— 症状是「回调跑了但 App
    // 还是退了」。
    let quit_confirm = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let confirming = quit_confirm.clone();
        app.on_back(move || {
            if confirming.swap(true, Ordering::AcqRel) {
                // 第二次：确认中 → **不消费** → Android 走默认行为 finish()，App 退出。
                //
                // ⚠️ 注意语义方向：**不消费 = 退出**。所以「确认退出」这一支
                // 要故意**不调** consume_back()。第一版我写反了（第二次消费），
                // 结果第一次按返回键就 finish() 掉、第二次按键根本没东西可按。
                log::info!("vireo-android: 返回键确认 → 退出");
            } else {
                // 第一次：**消费掉**，拦住 Android 的默认退出，App 留在原地。
                // 真实项目在这里弹自己的「退出游戏？」UI。
                vireo::platform::android_lifecycle::consume_back();
                log::info!("vireo-android: 收到返回键（已拦住退出，再按一次确认）");
            }
        });
    }

    let life = life.clone();

    // 触摸 → 按钮命中判定。Android 上点击只走 Touch 事件，鼠标按键状态恒 false
    // （详见 run 循环里那段注释）。
    //
    // ⚠️ **不能用 `mouse_left()`**：Android 上点击只走 `WindowEvent::Touch`，
    // **没有** `CursorMoved` / 鼠标按键事件（见 `window/mod.rs` 里「触摸点同时充当
    // 指针位置」那段注释）。vireo 只把触摸**位置**写进 `mouse_pos`，**不写鼠标
    // 按键状态** —— 所以 `mouse_left()` 在触摸设备上恒为 `false`。
    //
    // 触摸的两种正确读法（**都不需要**原子标志 + 回调跨线程搬运）：
    //
    // 1. **轮询（本例采用）** `win.touch_count()` / `win.touch_first()` / `win.touches()`
    //    —— 渲染线程每帧直接读内部缓存，无跨线程搬运、无回调注册。
    // 2. **回调** `app.on_touch(idx, ..)` —— 在 winit 线程派发，适合「事件驱动」
    //    场景（把 Started 记进原子标志，渲染循环消费）。
    //
    // 1 和 2 之前只能选 2，因为当时触摸**没有类型安全的读法**
    // （`InputState.touches` 字段虽为 `pub`，但类型是裸
    // `HashMap<u64, (f32, f32, Option<f64>)>`，无排序保证、无文档）。现在有了。

    // 打开就沉浸式：状态栏 + 导航栏都让给内容。
    #[cfg(target_os = "android")]
    {
        use vireo::platform::android::{SystemBars, WindowExtAndroid};
        if let Ok(win) = app.window_ref(&idx) {
            let _ = win.set_system_bars(SystemBars::ALL);
            let _ = win.set_keep_screen_on(true);
        }
    }

    // ---- 点左下角按钮：切 auto_pause（轮询触摸状态 + 边沿检测）----------------
    //
    // ⚠️ **轮询是「电平」语义，手指按住期间每帧都是 `Some`** —— 直接拿
    // `if let Some(tp) = win.touch_first()` 判定，会每帧重复触发（实测按钮
    // true→false→true 乱跳）。按钮要的是「按下那一瞬间」触发一次，所以必须自己
    // 做**边沿**检测：**本帧出现、上一帧没有**的 touch id = 刚按下的手指。
    //
    // 这就是 `touch_*` 轮询 API 与 `on_touch` 回调的本质分工：
    //   轮询 → 「此刻状态」，边沿要自己差分（省掉回调注册与跨线程搬运）
    //   回调 → 「事件流」，自带 Started/Ended 边沿，但要搬到渲染线程
    // 两条路都成立，按场景选。本例用轮询，因为按钮判定需要坐标 + 布局尺寸，
    // 两样都在渲染线程现成可读。
    let mut seen_ids: std::collections::HashSet<u64> = std::collections::HashSet::new();

    let mut wait_tried = false;

    // ---- 文件读写：随包资源 + 三个可写目录 ---------------------------------
    //
    // 安卓的「文件」有三个互不相通的世界，这是最常见的坑：
    //   1. 随包资源 —— **不是路径**，是 APK 这个 zip 里的一项，std::fs 读不到
    //   2. 内部私有目录 —— /data/user/0/<包名>/files，免权限
    //   3. 外部 app 专属 —— /storage/emulated/0/Android/data/<包名>/files，免权限
    //
    // 三者**都随卸载删除**。要让用户自己选存储位置得走 SAF，vireo 尚未实现。
    //
    // 桌面开发时同一份代码用 `set_asset_root` 指到本地目录即可 —— 这就是
    // `assets/` API 的全部意义：不是魔法，是把「资源根」显式化。
    let mut io_report = String::new();
    io_report.push_str(&probe_assets(&app));
    io_report.push_str(&probe_dirs(&app));
    // 同样打一份到 logcat：HUD 要肉眼读，logcat 能被 `adb logcat` / CI 抓，
    // 且真机验证时这是唯一可靠通道（模拟器多屏时 screencap 常常抓错屏）。
    for line in io_report.lines() {
        log::info!("vireo-android: io | {line}");
    }
    // 末级名字可改（默认取可执行文件名）。安卓下三个目录由系统从包名派生，
    // 这个名字在安卓上是 no-op —— 写在这里是为了让桌面行为可见。
    app.set_storage_name("vireo-android-template");

    // 随包贴图：证明 assets/logo.png 真的解码成功了（不是 missing 棋盘）。
    let asset_tex = app.load_texture_asset("logo.png");

    app.run(move |ctx| {
        let win = match ctx.app().window_ref(&idx) {
            Ok(w) => w,
            Err(_) => return true,
        };

        let fresh: Vec<vireo::input::TouchPoint> = {
            let pts = win.touches();
            let fresh: Vec<vireo::input::TouchPoint> = pts
                .iter()
                .copied()
                .filter(|p| !seen_ids.contains(&p.id))
                .collect();
            seen_ids = pts.iter().map(|p| p.id).collect();
            fresh
        };
        for tp in fresh {
            let m = tp.pos.logical();
            // 判定必须与绘制用**同一个**尺寸来源（`layout_size`），否则窗口改尺寸后
            // 按钮移动而点击区留在原地。
            let sz = win.layout_size();
            let btn_y = sz.height.dp.0 - 120.0;
            if m.0 >= 40.0 && m.0 <= 340.0 && m.1 >= btn_y && m.1 <= btn_y + 64.0 {
                let on = !ctx.app().auto_pause();
                ctx.app().set_auto_pause(on);
                log::info!(
                    "vireo-android: auto_pause = {on}（on = vireo 自动阻塞；\
                     切到 ON 后按 HOME，bg 应停止增长）"
                );
            } else {
                log::info!(
                    "vireo-android: touch#{} at logical ({:.0}, {:.0})，按钮在 y {:.0}..{:.0}",
                    tp.id,
                    m.0,
                    m.1,
                    btn_y,
                    btn_y + 64.0
                );
            }
        }
        if ctx.app().auto_pause() {
            life.auto_frames.fetch_add(1, Ordering::Relaxed);
        }

        // ---- 安全区三值 --------------------------------------------------
        #[cfg(target_os = "android")]
        let (safe, pad, ime): (Edges, Edges, Edges) = {
            use vireo::platform::android::WindowExtAndroid;
            (
                win.safe_area().into(),
                win.padding().into(),
                win.ime_insets().into(),
            )
        };
        #[cfg(not(target_os = "android"))]
        let (safe, pad, ime): (Edges, Edges, Edges) = Default::default();

        let size = win.layout_size();
        let (vw, vh) = (size.width.dp.0 as f32, size.height.dp.0 as f32);

        let mut batch = DrawBatch::new();

        // ---- auto_pause 切换按钮 ---------------------------------------------
        // 位置按布局尺寸算，别写死 —— 写死过一次（y=640..700）正好落在
        // 1138x640 的逻辑视口**外面**，按钮看不见也点不到。
        let gate_on = ctx.app().auto_pause();
        let (bx, by) = (40.0, vh - 120.0);
        draw_rectangle(&mut batch, Pos::new(bx, by), 300.0, 64.0, Some(GRAY));
        draw_text(
            &mut batch.texts,
            if gate_on {
                "auto_pause: ON"
            } else {
                "auto_pause: off"
            },
            Pos::new(bx + 16.0, by + 18.0),
            TextDef {
                font_size: 22.0,
                ..Default::default()
            },
            TextOverride::new(),
        );

        // 三圈：safe_area（最大可能遮挡）/ padding（当前可见遮挡）/ ime（键盘）
        ring(&mut batch, vw, vh, safe, RED);
        ring(&mut batch, vw, vh, pad, YELLOW);
        ring(&mut batch, vw, vh, ime, GREEN);

        // ---- 文本输入框（键盘弹出时上移）----------------------------------
        let fw = vw - 160.0;
        let fx = 80.0;
        let fy = 160.0 + ime.b;
        draw_rectangle(&mut batch, Pos::new(fx, fy), fw, 72.0, Some(GRAY));

        let m = win.mouse_pos().logical();
        let (mx, my) = (m.0 as f32, m.1 as f32);
        let focused = mx >= fx && mx <= fx + fw && my >= fy && my <= fy + 72.0;

        let (typed, cursor) = {
            #[cfg(target_os = "android")]
            {
                use vireo::platform::android::WindowExtAndroid;
                match win.text_input_state() {
                    Some(s) => (s.text.clone(), s.selection.end),
                    None => (String::new(), 0),
                }
            }
            #[cfg(not(target_os = "android"))]
            {
                (String::new(), 0usize)
            }
        };

        let shown: &str = if focused {
            typed.as_str()
        } else {
            "点我聚焦"
        };
        draw_text(
            &mut batch.texts,
            shown,
            Pos::new(fx + 16.0, fy + 16.0),
            TextDef {
                font_size: 28.0,
                ..Default::default()
            },
            TextOverride::new(),
        );

        // 系统不画光标（`TextInputState` 只给文本 + selection），自己画一根。
        // 宽度是粗略估算；真要精确得用 `gpu.measure_text`。
        if focused {
            let approx = 16.0 + cursor as f32 * 17.0;
            draw_rectangle(
                &mut batch,
                Pos::new(fx + approx, fy + 14.0),
                3.0,
                30.0,
                Some(WHITE),
            );
        }

        // ---- HUD ------------------------------------------------------------
        let mut hud = format!("fps {:.0}   {vw:.0}x{vh:.0}\n", ctx.fps());
        hud.push_str(&format!(
            "safe    {:.0},{:.0}  {:.0},{:.0}\n",
            safe.l, safe.t, safe.r, safe.b
        ));
        hud.push_str(&format!(
            "padding {:.0},{:.0}  {:.0},{:.0}\n",
            pad.l, pad.t, pad.r, pad.b
        ));
        hud.push_str(&format!(
            "ime     {:.0},{:.0}  {:.0},{:.0}\n",
            ime.l, ime.t, ime.r, ime.b
        ));
        // 生命周期 / 闸门观测。按 Home 回桌面再回来 → paused+1 且 stopped+1；
        // 划掉最近任务 → destroyed+1。
        hud.push_str(&format!(
            "life    paused={} stopped={} destroyed={}\n",
            life.paused.load(Ordering::Relaxed),
            life.stopped.load(Ordering::Relaxed),
            life.destroyed.load(Ordering::Relaxed),
        ));
        // 真实刷新率（Android 专用）。音游判定校准必须用它 —— winit 在 Android 上
        // 给不出这个值（`video_modes()` 返回硬编码的 60Hz），详见
        // `WindowExtAndroid::display_refresh_rate` 的文档。
        //
        // ⚠️ 这是【同步 JNI 查询】不是缓存读，别放进每帧热路径；这里每帧重读
        // 是为了演示 DRR 设备上刷新率会变（真实项目在 init + on_resumed 读）。
        #[cfg(target_os = "android")]
        {
            use vireo::platform::android::WindowExtAndroid;
            hud.push_str(&format!(
                "refresh {}\n",
                match win.display_refresh_rate() {
                    Some(h) => format!("{h:.1} Hz"),
                    None => "N/A".to_string(),
                }
            ));
        }
        hud.push_str(&format!(
            "gate    auto={} bg={} auto_frames={}\n",
            if ctx.app().auto_pause() { "ON " } else { "off" },
            life.bg_frames.load(Ordering::Relaxed),
            life.auto_frames.load(Ordering::Relaxed),
        ));
        hud.push_str(&format!(
            "wait    paused_wait={} until_ok={}\n",
            life.waited.load(Ordering::Relaxed),
            life.wait_until_ok.load(Ordering::Relaxed),
        ));
        // 返回键二段确认状态：按一次变 1（收到但未消费），再按一次消费并退出。
        hud.push_str(&format!(
            "back    confirm={}（按返回键：第一次=收到，第二次=退出）",
            if quit_confirm.load(Ordering::Relaxed) {
                "1 已收到"
            } else {
                "0"
            },
        ));
        hud.push_str("gate    点左下角方块 = 切 auto_pause");
        hud.push_str(&format!("text    {typed:?}"));
        // 文件读写探测结果。安卓上重点看这三行：
        //   assets  有值  = APK 里的 src/main/assets/ 读到了
        //   dirs    三行   = 内部/外部/缓存目录都拿到了
        //   save    roundtrip = 目录真的可写（不只是路径算得出来）
        hud.push('\n');
        hud.push_str(&io_report);
        // emoji：曲名/故事板要用，得确认能画出来（不是豆腐块）
        let mut emoji_probe = String::from("\nemoji");
        // 逐行测：单色 / 旗帜 / ZWJ 序列 / 肤色修饰
        for (label, s) in [
            ("fox", "🦊"),
            ("note", "🎵"),
            ("grin", "😀"),
            ("flagCN", "🇨🇳"),
            ("zwj-family", "👨‍👩‍👧"),
            ("skin", "👍🏽"),
        ] {
            emoji_probe.push_str(&format!("  {label:<10} {s}"));
            if label != "skin" {
                emoji_probe.push('\n');
            }
        }
        draw_text(
            &mut batch.texts,
            &emoji_probe,
            Pos::new(80.0, 330.0),
            TextDef {
                font_size: 20.0,
                ..Default::default()
            },
            TextOverride::new(),
        );

        // 字体加载摘要（Android 上 logcat 不可靠，看画面）
        #[cfg(target_os = "android")]
        {
            // 字体加载摘要（Android 上 logcat 不可靠，看画面）
            let mut line = String::new();
            match vireo::text::android_fonts::report() {
                Some(r) => line.push_str(&format!("fonts   {r}\n")),
                None => line.push_str("fonts   (install() 未运行)\n"),
            }
            draw_text(
                &mut batch.texts,
                &line,
                Pos::new(80.0, 500.0),
                TextDef {
                    font_size: 16.0,
                    ..Default::default()
                },
                TextOverride::new(),
            );
        }

        draw_text(
            &mut batch.texts,
            &hud,
            Pos::new(80.0, 60.0),
            TextDef {
                font_size: 20.0,
                ..Default::default()
            },
            TextOverride::new(),
        );

        // ---- 接管暂停时机后的三件事 ------------------------------------------
        // 因为上面 `set_auto_pause(false)` 关掉了 vireo 的闸门，这里能拿到控制权。
        //
        // 1. 用户主动关闭（划掉最近任务）→ 退出。这是 auto_pause=false 后
        //    「后台空转」唯一的终止条件：`onPause` / `onStop` 都收不到。
        if ctx.app().is_destroyed() {
            log::info!("vireo-android: 用户主动关闭，退出循环");
            return false;
        }
        // 2. 后台：仍然 tick（证明闸门关掉了），但**不画** —— 窗口不可见时
        //    acquire/present 纯属浪费，而且会让 Android 更早杀进程。
        if ctx.app().is_paused() {
            life.bg_frames.fetch_add(1, Ordering::Relaxed);
            if !ctx.app().is_stopped() {
                // 半透明弹窗：还可见，继续画。真正不可见才停 —— 这正是
                // 「短暂中断 vs 彻底离开」的价值所在。
                // 随包贴图：assets/logo.png 解码成功才会画出真图；读不到时 vireo 给出
                // ffcc00/6699ff 棋盘（一眼能分辨「读不到」与「读到了」）。
                if let Ok(t) = ctx.app().texture(asset_tex) {
                    let h = 96.0_f32.min(vh * 0.16);
                    let s = h / t.height.max(1) as f32;
                    batch.set_texture(Some(&*t));
                    draw_rectangle(
                        &mut batch,
                        Pos::new(vw - t.width as f32 * s - pad.r - 8.0, pad.t + 8.0),
                        t.width as f32 * s,
                        h,
                        Some(WHITE),
                    );
                    // 贴图画完必须清画笔状态，否则后面的形状会继续采样这张图
                    batch.set_texture(None);
                }

                win.draw(BLACK, &[&batch]);
            }
            return true;
        }
        // 3. 想要「这一帧跑完再彻底停」时，可以用 wait_while_paused()。
        //    这里演示它的可打断性：返回后 waited+1。只在「刚恢复」的第一帧调一次，
        //    否则每帧都白等一趟。
        if life.paused.load(Ordering::Relaxed) > life.waited.load(Ordering::Relaxed) {
            ctx.app().wait_while_paused();
            life.waited.fetch_add(1, Ordering::Relaxed);
            log::info!("vireo-android: wait_while_paused 返回（可打断，未挂死）");
        }
        // 4. wait_until + notify_waiters 的往返：目标条件由后台线程在 3 秒后置位。
        //    只跑一次。条件变化本身**不会**自动唤醒（平台状态才会），
        //    所以这条同时验证了「自定义条件必须自己 notify」这条约束。
        if !wait_goal.load(Ordering::Relaxed) && !wait_tried {
            wait_tried = true;
            let t0 = std::time::Instant::now();
            // 故意先等 4 秒再进 wait：让条件在**进入等待之前**就成立 ——
            // 这是「通知早于等待不得丢唤醒」的真机版验证（单测里有对应那条）。
            std::thread::sleep(std::time::Duration::from_millis(4000));
            let ok = ctx
                .app()
                .wait_until(Some(std::time::Duration::from_secs(2)), || {
                    wait_goal.load(Ordering::Relaxed)
                });
            let dt = t0.elapsed();
            if ok {
                life.wait_until_ok.fetch_add(1, Ordering::Relaxed);
                log::info!(
                    "vireo-android: wait_until 返回 true，耗时 {dt:?} \
                     （条件已成立 → 立即返回，未白等满 timeout）"
                );
            } else {
                log::warn!("vireo-android: wait_until 超时/被打断（{dt:?}）");
            }
        }

        win.draw(BLACK, &[&batch]);

        // ---- 焦点管理放帧末：早一帧调会被部分 ROM 吞掉 ----------------------
        #[cfg(target_os = "android")]
        {
            use vireo::platform::android::{InputType, WindowExtAndroid};
            if focused {
                let _ = win.set_input_type(InputType::Text);
                let _ = win.show_soft_input();
            } else {
                let _ = win.hide_soft_input();
            }
        }

        true
    })
    .await
    .unwrap();
}

/// 探测随包资源（只读）。返回 HUD 用的一行文本。
///
/// 真机验证点：`assets/logo.png` 与 `assets/notes/hello.txt` 放在
/// `src/main/assets/`（AGP 默认打进 APK，无需改 build.gradle）。
/// 若这里报 `AssetNotFound`，先确认文件在**那个**目录，而不是项目根。
fn probe_assets(app: &vireo::App) -> String {
    let a = app.assets();
    let mut s = format!("assets  root={:?} ", a.root());
    match a.read("notes/hello.txt") {
        Ok(b) => {
            let first = String::from_utf8_lossy(&b)
                .lines()
                .next()
                .unwrap_or("")
                .to_string();
            s.push_str(&format!("hello={}B「{first}」", b.len()));
        }
        Err(e) => s.push_str(&format!("hello=ERR({e})")),
    }
    match a.list("") {
        Ok(mut n) => {
            n.sort();
            s.push_str(&format!(" list={n:?}"));
        }
        Err(e) => s.push_str(&format!(" list=ERR({e})")),
    }
    // 收尾换行：两段 probe 报告靠 '\n' 分隔，缺了会在 HUD 上粘成一行
    s.push('\n');
    s
}

/// 探测三个可写目录 + 真写一次再读回。
fn probe_dirs(app: &vireo::App) -> String {
    let mut s = format!("dirs   name={:?}\n", app.storage_name());
    for (label, d) in [("data", app.data_dir()), ("cache", app.cache_dir())] {
        match d {
            Ok(p) => match vireo::storage::ensure_dir(&p) {
                Ok(()) => s.push_str(&format!("dir    {label:<8} {}\n", p.display())),
                Err(e) => s.push_str(&format!("dir    {label:<8} mkdir ERR({e})\n")),
            },
            Err(e) => s.push_str(&format!("dir    {label:<8} ERR({e})\n")),
        }
    }
    match app.external_data_dir() {
        Some(p) => s.push_str(&format!("dir    external {}\n", p.display())),
        None => s.push_str("dir    external 不可用\n"),
    }

    // 往返：写一个文件再读回来，证明目录真的可写（不只是路径能算出来）
    let rt: Result<String, String> = (|| {
        let dir = app.data_dir().map_err(|e| e.to_string())?.join("io-demo");
        vireo::storage::ensure_dir(&dir).map_err(|e| e.to_string())?;
        let f = dir.join("save.json");
        std::fs::write(&f, br#"{"ok":true}"#).map_err(|e| e.to_string())?;
        std::fs::read_to_string(&f).map_err(|e| e.to_string())
    })();
    match rt {
        Ok(v) => s.push_str(&format!("save   roundtrip {v}")),
        Err(e) => s.push_str(&format!("save   ERR({e})")),
    }
    s
}

/// 画一圈安全区：四条边各一个半透明矩形。
fn ring(batch: &mut DrawBatch, vw: f32, vh: f32, e: Edges, color: Color) {
    let c = color.with_alpha(0.28);
    if e.r > 0.0 {
        draw_rectangle(batch, Pos::new(vw - e.r, 0.0), e.r, vh, Some(c));
    }
    if e.b > 0.0 {
        draw_rectangle(batch, Pos::new(0.0, vh - e.b), vw, e.b, Some(c));
    }
    if e.l > 0.0 {
        draw_rectangle(batch, Pos::new(0.0, 0.0), e.l, vh, Some(c));
    }
    if e.t > 0.0 {
        draw_rectangle(batch, Pos::new(0.0, 0.0), vw, e.t, Some(c));
    }
}
