//! 系统振动（纯免费函数，无 App/窗口状态）。
//!
//! 和剪贴板同款理由挂免费函数：振动器是进程级 OS 资源，读写它不需要任何
//! vireo 句柄（`App` 包一层只会是纯透传）。见 `crate::clipboard` 头注的讨论。
//!
//! # 接口
//!
//! - [`has_vibrator`]：有没有马达（电视/模拟器=false，非错误）。
//! - [`vibrate_once`]：单次（毫秒 + 可选强度，`None` = 系统默认）。
//! - [`vibrate_predefined`]：四种系统预置触感（和系统 UI 手感一致）。
//! - [`vibrate_pattern`]：自定义波形（开/关时间序列 + 可选强度 + 可选循环）。
//! - [`cancel_vibration`]：停掉当前振动（fire-and-forget，无马达时直接 `Ok`）。
//!
//! # 平台行为
//!
//! - **Android**：纯 JNI（`VibratorManager`→`Vibrator`……实际直接取 `vibrator`
//!   服务，少一跳，行为与默认振动器一致），**需要 manifest 声明 `VIBRATE` 权限**
//!   （普通权限，安装即授予；没声明调用直接抛 `SecurityException`）。
//!   无需改 Java。任意线程可调。
//! - **其它平台**：[`VireoError::Unsupported`]（读状态 `has_vibrator` 返回
//!   `false`；`cancel` 也返回 `Unsupported` —— 统一「本平台不会震」的信号，
//!   调用方只认 `has_vibrator` 做门即可）。
//!
//! # 模拟器警告
//!
//! 模拟器（MuMu 等）**没有马达**：`has_vibrator()=false`，`vibrate_*` 一律
//! `Err("no vibrator")`。设备上只能验证「不崩 + 路径正确」，真震感（强度、
//! 波形节奏）必须物理手机。`long[]/int[]` 数组那段 JNI 在无马达设备上跑不到
//! （vibrator 为空提前返回），是残留风险，已在此注明。
//!
//! # 波形语义（`vibrate_pattern`）
//!
//! - `timings_ms[0]` = 起始延迟（通常 0），之后开/关交替，单位毫秒。
//! - `amplitudes` 给了就必须和 `timings_ms` 等长（关段的强度会被忽略，
//!   填什么都行，惯例 0）；`None` = 全默认强度。
//! - `repeat_at` = 从第几个下标开始循环，`None` = 不循环。
//! - 校验（空数组/长度 mismatch/下标越界）在碰 JNI 前就 `Err`，纯函数单测覆盖。

use crate::error::VireoError;

/// 系统预置触感（`VibrationEffect.createPredefined`，和系统 UI 手感一致）。
///
/// 游戏里 `Tick` 最常用（轻点确认），`HeavyClick` 给重打击，`DoubleClick` 给双段。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VibrateKind {
    /// 轻滴答（UI 确认首选）。
    Tick,
    /// 普通点击。
    Click,
    /// 重点击（打击反馈）。
    HeavyClick,
    /// 双击（双段确认）。
    DoubleClick,
}

impl VibrateKind {
    /// 对应的 `VibrationEffect.EFFECT_*` 字段名（运行时反射读，不 hardcode 值）。
    /// 安卓生产代码 + 全平台单测用。
    #[cfg(any(target_os = "android", test))]
    pub(crate) fn effect_field_name(self) -> &'static str {
        match self {
            VibrateKind::Tick => "EFFECT_TICK",
            VibrateKind::Click => "EFFECT_CLICK",
            VibrateKind::HeavyClick => "EFFECT_HEAVY_CLICK",
            VibrateKind::DoubleClick => "EFFECT_DOUBLE_CLICK",
        }
    }
}

/// 有没有马达。`false` 是正常设备状态（电视/模拟器），不是错误。
pub fn has_vibrator() -> bool {
    #[cfg(target_os = "android")]
    {
        has_vibrator_android()
    }
    #[cfg(not(target_os = "android"))]
    {
        false
    }
}

/// 单次振动（毫秒 + 可选强度 1~255，`None` = 系统默认）。
///
/// 无马达设备返回 `Err`（调用方启动时查一次 [`has_vibrator`] 做门，而不是
/// 每次 hit 都来一次错）。
pub fn vibrate_once(ms: u32, amplitude: Option<u8>) -> Result<(), VireoError> {
    #[cfg(target_os = "android")]
    {
        vibrate_once_android(ms, amplitude)
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = (ms, amplitude);
        Err(VireoError::Unsupported("vibration"))
    }
}

/// 系统预置触感。
pub fn vibrate_predefined(kind: VibrateKind) -> Result<(), VireoError> {
    #[cfg(target_os = "android")]
    {
        vibrate_predefined_android(kind)
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = kind;
        Err(VireoError::Unsupported("vibration"))
    }
}

/// 自定义波形（见模块文档的语义节）。
///
/// 校验先于平台分发：垃圾输入在所有平台上都报同样的参数错，不会因为平台不同
/// 而行为分叉。
pub fn vibrate_pattern(
    timings_ms: &[u64],
    amplitudes: Option<&[u8]>,
    repeat_at: Option<usize>,
) -> Result<(), VireoError> {
    validate_pattern(timings_ms, amplitudes, repeat_at)?;
    #[cfg(target_os = "android")]
    {
        vibrate_pattern_android(timings_ms, amplitudes, repeat_at)
    }
    #[cfg(not(target_os = "android"))]
    {
        Err(VireoError::Unsupported("vibration"))
    }
}

/// 停掉当前振动。无马达时直接 `Ok`（取消本来就是 fire-and-forget）。
pub fn cancel_vibration() -> Result<(), VireoError> {
    #[cfg(target_os = "android")]
    {
        cancel_android()
    }
    #[cfg(not(target_os = "android"))]
    {
        Err(VireoError::Unsupported("vibration"))
    }
}

// ---- 纯逻辑（无 cfg，全平台可测） --------------------------------------------------

/// 波形参数校验：空数组/长度 mismatch/循环下标越界。
pub(crate) fn validate_pattern(
    timings: &[u64],
    amplitudes: Option<&[u8]>,
    repeat_at: Option<usize>,
) -> Result<(), VireoError> {
    if timings.is_empty() {
        return Err(VireoError::Other("vibration pattern is empty".to_string()));
    }
    if let Some(a) = amplitudes
        && a.len() != timings.len()
    {
        return Err(VireoError::Other(format!(
            "vibration amplitudes len {} != timings len {}",
            a.len(),
            timings.len()
        )));
    }
    if let Some(r) = repeat_at
        && r >= timings.len()
    {
        return Err(VireoError::Other(format!(
            "vibration repeat_at {r} out of range (len {})",
            timings.len()
        )));
    }
    Ok(())
}

// ---- Android ------------------------------------------------------------------------

/// `vibrator` 系统服务对象。`None` = 服务不存在（当无马达处理）。
#[cfg(target_os = "android")]
fn vibrator_object<'local>(
    env: &mut jni::Env<'local>,
    activity: &jni::objects::JObject<'_>,
) -> Result<Option<jni::objects::JObject<'local>>, crate::platform::android::JniErr> {
    use crate::platform::android::JniErr;
    use jni::{jni_sig, jni_str};
    let name = env.new_string("vibrator").map_err(JniErr::from)?;
    let svc = env
        .call_method(
            activity,
            jni_str!("getSystemService"),
            jni_sig!("(Ljava/lang/String;)Ljava/lang/Object;"),
            &[jni::objects::JValue::Object(&name.into())],
        )
        .map_err(JniErr::from)?
        .l()
        .map_err(JniErr::from)?;
    if svc.as_raw().is_null() {
        return Ok(None);
    }
    Ok(Some(svc))
}

/// 拿到可用马达；无马达返回 `Unsupported`（调用方外层转成对用户友好的文案）。
#[cfg(target_os = "android")]
fn require_vibrator<'local>(
    env: &mut jni::Env<'local>,
    activity: &jni::objects::JObject<'_>,
) -> Result<jni::objects::JObject<'local>, crate::platform::android::JniErr> {
    use crate::platform::android::JniErr;
    use jni::{jni_sig, jni_str};
    let svc = vibrator_object(env, activity)?;
    let vib = match svc {
        Some(v) => v,
        None => return Err(JniErr::Unsupported("no vibrator")),
    };
    let has = env
        .call_method(&vib, jni_str!("hasVibrator"), jni_sig!("()Z"), &[])
        .map_err(JniErr::from)?
        .z()
        .map_err(JniErr::from)?;
    if !has {
        return Err(JniErr::Unsupported("no vibrator"));
    }
    Ok(vib)
}

/// JNI 层错误转公开错误：无马达是正常设备状态（文案直说），其余是真故障。
/// 调用方外层 `.map_err(vib_err)` 统一收口。
#[cfg(target_os = "android")]
fn vib_err(e: crate::platform::android::JniErr) -> VireoError {
    match e {
        crate::platform::android::JniErr::Unsupported(_) => {
            VireoError::Other("no vibrator on this device".to_string())
        }
        e => VireoError::Other(format!("vibration failed: {e:?}")),
    }
}

#[cfg(target_os = "android")]
fn vibrator_effect_class<'local>(
    env: &mut jni::Env<'local>,
) -> Result<jni::objects::JClass<'local>, crate::platform::android::JniErr> {
    use crate::platform::android::JniErr;
    use jni::jni_str;
    env.find_class(jni_str!("android/os/VibrationEffect"))
        .map_err(JniErr::from)
}

#[cfg(target_os = "android")]
fn has_vibrator_android() -> bool {
    use crate::platform::android::with_activity;
    let Some(app) = crate::app::android_app() else {
        return false;
    };
    // with_activity 自带 attach，这里不再包一层（嵌套 attach 虽无害但是绕路）。
    with_activity(app, |env, activity| {
        let vib = match vibrator_object(env, activity) {
            Ok(v) => v,
            Err(_) => return Ok(false),
        };
        let Some(vib) = vib else { return Ok(false) };
        use jni::{jni_sig, jni_str};
        let has = env
            .call_method(&vib, jni_str!("hasVibrator"), jni_sig!("()Z"), &[])
            .map_err(crate::platform::android::JniErr::from)?
            .z()
            .map_err(crate::platform::android::JniErr::from)?;
        Ok(has)
    })
    .unwrap_or(false)
}

#[cfg(target_os = "android")]
fn vibrate_once_android(ms: u32, amplitude: Option<u8>) -> Result<(), VireoError> {
    use crate::platform::android::{JniErr, with_activity};
    use jni::{jni_sig, jni_str};
    let app = crate::app::android_app().ok_or_else(|| {
        VireoError::Other(
            "AndroidApp unavailable: vibration needs the #[vireo::main] android_main entry"
                .to_string(),
        )
    })?;
    with_activity(app, |env, activity| {
        let vib = require_vibrator(env, activity)?;
        let cls = vibrator_effect_class(env)?;
        // `DEFAULT_AMPLITUDE = -1`（AOSP 稳定值）：传 -1 用系统默认强度。
        let amp: i32 = amplitude.map(|a| a as i32).unwrap_or(-1);
        let effect = env
            .call_static_method(
                cls,
                jni_str!("createOneShot"),
                jni_sig!("(JI)Landroid/os/VibrationEffect;"),
                &[
                    jni::objects::JValue::Long(ms as i64),
                    jni::objects::JValue::Int(amp),
                ],
            )
            .map_err(JniErr::from)?
            .l()
            .map_err(JniErr::from)?;
        if effect.as_raw().is_null() {
            return Err(JniErr::Null("VibrationEffect"));
        }
        env.call_method(
            &vib,
            jni_str!("vibrate"),
            jni_sig!("(Landroid/os/VibrationEffect;)V"),
            &[jni::objects::JValue::Object(&effect)],
        )
        .map_err(JniErr::from)?;
        Ok(())
    })
    .map_err(vib_err)
}

#[cfg(target_os = "android")]
fn vibrate_predefined_android(kind: VibrateKind) -> Result<(), VireoError> {
    use crate::platform::android::{JniErr, with_activity};
    use jni::{jni_sig, jni_str};
    let app = crate::app::android_app().ok_or_else(|| {
        VireoError::Other(
            "AndroidApp unavailable: vibration needs the #[vireo::main] android_main entry"
                .to_string(),
        )
    })?;
    with_activity(app, |env, activity| {
        let vib = require_vibrator(env, activity)?;
        let cls = vibrator_effect_class(env)?;
        // EFFECT_* 常量运行时反射读（值不 hardcode，见 ACTION_OPEN_DOCUMENT_TREE 的教训）。
        // 名字是运行时分支（match 结果），经 JNIString 转 JNIStr（`jni_str!` 只要字面量）。
        let fname = jni::strings::JNIString::new(kind.effect_field_name());
        let effect_id = env
            .get_static_field(cls, fname.as_ref(), jni_sig!("I"))
            .map_err(JniErr::from)?
            .i()
            .map_err(JniErr::from)?;
        // JClass 非 Copy：查两次（find_class 便宜，振动调用低频）。
        let cls2 = vibrator_effect_class(env)?;
        let effect = env
            .call_static_method(
                cls2,
                jni_str!("createPredefined"),
                jni_sig!("(I)Landroid/os/VibrationEffect;"),
                &[jni::objects::JValue::Int(effect_id)],
            )
            .map_err(JniErr::from)?
            .l()
            .map_err(JniErr::from)?;
        if effect.as_raw().is_null() {
            return Err(JniErr::Null("VibrationEffect"));
        }
        env.call_method(
            &vib,
            jni_str!("vibrate"),
            jni_sig!("(Landroid/os/VibrationEffect;)V"),
            &[jni::objects::JValue::Object(&effect)],
        )
        .map_err(JniErr::from)?;
        Ok(())
    })
    .map_err(vib_err)
}

#[cfg(target_os = "android")]
fn vibrate_pattern_android(
    timings_ms: &[u64],
    amplitudes: Option<&[u8]>,
    repeat_at: Option<usize>,
) -> Result<(), VireoError> {
    use crate::platform::android::{JniErr, with_activity};
    use jni::{jni_sig, jni_str};
    let app = crate::app::android_app().ok_or_else(|| {
        VireoError::Other(
            "AndroidApp unavailable: vibration needs the #[vireo::main] android_main entry"
                .to_string(),
        )
    })?;
    with_activity(app, |env, activity| {
        let vib = require_vibrator(env, activity)?;
        let cls = vibrator_effect_class(env)?;
        let timings: Vec<i64> = timings_ms.iter().map(|&t| t as i64).collect();
        let tarr = env.new_long_array(timings.len()).map_err(JniErr::from)?;
        tarr.set_region(env, 0, &timings).map_err(JniErr::from)?;
        // 不循环 = -1（AOSP 稳定值）。
        let repeat: i32 = repeat_at.map(|r| r as i32).unwrap_or(-1);
        let effect = match amplitudes {
            Some(a) => {
                let amps: Vec<i32> = a.iter().map(|&x| x as i32).collect();
                let aarr = env.new_int_array(amps.len()).map_err(JniErr::from)?;
                aarr.set_region(env, 0, &amps).map_err(JniErr::from)?;
                env.call_static_method(
                    cls,
                    jni_str!("createWaveform"),
                    jni_sig!("([J[II)Landroid/os/VibrationEffect;"),
                    &[
                        jni::objects::JValue::Object(&tarr.into()),
                        jni::objects::JValue::Object(&aarr.into()),
                        jni::objects::JValue::Int(repeat),
                    ],
                )
            }
            None => env.call_static_method(
                cls,
                jni_str!("createWaveform"),
                jni_sig!("([JI)Landroid/os/VibrationEffect;"),
                &[
                    jni::objects::JValue::Object(&tarr.into()),
                    jni::objects::JValue::Int(repeat),
                ],
            ),
        }
        .map_err(JniErr::from)?
        .l()
        .map_err(JniErr::from)?;
        if effect.as_raw().is_null() {
            return Err(JniErr::Null("VibrationEffect"));
        }
        env.call_method(
            &vib,
            jni_str!("vibrate"),
            jni_sig!("(Landroid/os/VibrationEffect;)V"),
            &[jni::objects::JValue::Object(&effect)],
        )
        .map_err(JniErr::from)?;
        Ok(())
    })
    .map_err(vib_err)
}

#[cfg(target_os = "android")]
fn cancel_android() -> Result<(), VireoError> {
    use crate::platform::android::{JniErr, with_activity};
    use jni::{jni_sig, jni_str};
    let app = crate::app::android_app().ok_or_else(|| {
        VireoError::Other(
            "AndroidApp unavailable: vibration needs the #[vireo::main] android_main entry"
                .to_string(),
        )
    })?;
    with_activity(app, |env, activity| {
        // 无马达 = 没东西可停，直接 Ok（取消是 fire-and-forget）。
        let vib = match vibrator_object(env, activity).map_err(JniErr::from)? {
            Some(v) => v,
            None => return Ok(()),
        };
        env.call_method(&vib, jni_str!("cancel"), jni_sig!("()V"), &[])
            .map_err(JniErr::from)?;
        Ok(())
    })
    .map_err(|e| VireoError::Other(format!("vibration cancel failed: {e:?}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effect_names_cover_all_kinds() {
        assert_eq!(VibrateKind::Tick.effect_field_name(), "EFFECT_TICK");
        assert_eq!(VibrateKind::Click.effect_field_name(), "EFFECT_CLICK");
        assert_eq!(
            VibrateKind::HeavyClick.effect_field_name(),
            "EFFECT_HEAVY_CLICK"
        );
        assert_eq!(
            VibrateKind::DoubleClick.effect_field_name(),
            "EFFECT_DOUBLE_CLICK"
        );
    }

    #[test]
    fn pattern_validation_rejects_garbage() {
        assert!(validate_pattern(&[], None, None).is_err());
        assert!(validate_pattern(&[0, 100], Some(&[1]), None).is_err());
        assert!(validate_pattern(&[0, 100], None, Some(2)).is_err());
        assert!(validate_pattern(&[0, 100], None, Some(99)).is_err());
        assert!(validate_pattern(&[0], Some(&[]), None).is_err());
    }

    #[test]
    fn pattern_validation_accepts_shapes() {
        assert!(validate_pattern(&[0], None, None).is_ok());
        assert!(validate_pattern(&[0, 100, 50], None, None).is_ok());
        assert!(validate_pattern(&[0, 100, 50], Some(&[0, 255, 0]), None).is_ok());
        assert!(validate_pattern(&[0, 100, 50], Some(&[0, 255, 0]), Some(1)).is_ok());
        assert!(validate_pattern(&[0, 100], None, Some(0)).is_ok());
    }

    #[test]
    fn desktop_stubs_do_not_panic() {
        // 非安卓：查询 false、动作 Unsupported，不 panic。
        #[cfg(not(target_os = "android"))]
        {
            assert!(!has_vibrator());
            assert!(matches!(
                vibrate_once(50, None),
                Err(VireoError::Unsupported(_))
            ));
            assert!(matches!(
                vibrate_predefined(VibrateKind::Tick),
                Err(VireoError::Unsupported(_))
            ));
            // 校验先于平台分发：垃圾参数在桌面也报参数错，不是 Unsupported。
            assert!(matches!(
                vibrate_pattern(&[], None, None),
                Err(VireoError::Other(_))
            ));
            assert!(matches!(
                vibrate_pattern(&[0, 100], None, None),
                Err(VireoError::Unsupported(_))
            ));
            assert!(matches!(
                cancel_vibration(),
                Err(VireoError::Unsupported(_))
            ));
        }
    }
}
