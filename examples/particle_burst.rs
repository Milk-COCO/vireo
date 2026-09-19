//! GPU 粒子演示：`ParticlePool` 常驻＋`draw_particles` 每帧一次推送，
//! 动画积分（`pos += vel*age`＋fade 包络）全在 VS 按 `camera.time` 算，
//! 存活期 CPU 只做 spawn/kill（无逐帧 transform/color 更新）。
//!
//! - 程序化 sprite atlas（单纹理四象限：软盘/硬方/圆环/十字）＋逐粒子 `uv_rect`
//!   （象限 UV 内缝侧内收半 texel：exact 边界 bilinear 会渗邻象限，粒子边缘多 1px 线）
//! - 空格：在鼠标位置按当前模式发射（1 radial burst / 2 ring / 3 rain）
//! - T：冻结粒子时钟（0 号钟 `set_clock_scale(0)`，动画暂停，证明时间由 GPU 统一驱动）
//! - M：fragment-only 自定义材质开关（`local_pos` 条带 tint，走粒子 instance 管线）
//! - B：粒子批切加色混合（`set_blend_state`，火光；黑底稀疏粒子下与 alpha 肉眼难分，看互叠处）
//! - G：压力档（一次 30000，粒子层仍 1 dc）/ +/-：持续负载目标（immortal 顶到数）
//! - C：清池 / H：HUD 文字开关
//! - HUD：存活数 / FPS / draw calls / 粒子时钟 / build-enc-acq-pres（EMA 平滑）
//!
//! 性能写法（大 N 必读）：
//! - batch 跨帧复用＋每帧 `clear()`，**不要每帧 `DrawBatch::new()`**——新鲜 batch
//!   全小容量，我设备上 18 万实例下重新 mmap＋首次触碰缺页约 10ms，复用只要 3ms。
//!   `draw_particles` 内有 `reserve`，但 reserve 修不了缺页，只能消几何拷贝。
//! - sweep 降频（本示例每 30 帧）：到期未扫的槽位照推照传，VS 按 age 照剔除不可见，
//!   sweep 只是内存回收，均摊后约 0.02ms。
//!
//! 运行：`cargo run --release --example particle_burst`（debug 模式 CPU 测很慢，测性能务必 release）

use std::sync::{Arc, Mutex};
use vireo::prelude::*;

fn rand01(state: &mut u64) -> f32 {
    *state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    // 取高 32 位：值域 [0, 1]（>> 33 会只剩 31 位 → [0, 0.5)，曾导致 burst 只有下半圆）
    ((*state >> 32) as f32) / (u32::MAX as f32)
}

/// 64×64 sprite atlas（Rgba8）：四象限 32×32，依次软盘/硬方/圆环/十字。
/// 全象限翻转对称，UV 上下方向无需操心。
fn sprite_atlas() -> Vec<u8> {
    let mut px = vec![0u8; 64 * 64 * 4];
    for y in 0..64 {
        for x in 0..64 {
            let qx = x / 32;
            let qy = y / 32;
            let lx = (x % 32) as f32 - 15.5;
            let ly = (y % 32) as f32 - 15.5;
            let d = (lx * lx + ly * ly).sqrt() / 16.0;
            let a = match (qx, qy) {
                (0, 0) => (1.0 - d).clamp(0.0, 1.0),
                (1, 0) => 1.0,
                (0, 1) => (1.0 - ((d - 0.6).abs() / 0.14).clamp(0.0, 1.0)).clamp(0.0, 1.0),
                _ => {
                    let bar = (1.0 - (lx.abs() / 4.0).clamp(0.0, 1.0))
                        .max(1.0 - (ly.abs() / 4.0).clamp(0.0, 1.0));
                    (bar - d * 0.35).clamp(0.0, 1.0)
                }
            };
            let o = (y * 64 + x) * 4;
            px[o] = 255;
            px[o + 1] = 255;
            px[o + 2] = 255;
            px[o + 3] = (a * 255.0) as u8;
        }
    }
    px
}

const HALF_TEXEL: f32 = 1.0 / 128.0;
const QUADS: [[f32; 4]; 4] = [
    [0.0, 0.0, 0.5 - HALF_TEXEL, 0.5 - HALF_TEXEL],
    [0.5 + HALF_TEXEL, 0.0, 1.0, 0.5 - HALF_TEXEL],
    [0.0, 0.5 + HALF_TEXEL, 0.5 - HALF_TEXEL, 1.0],
    [0.5 + HALF_TEXEL, 0.5 + HALF_TEXEL, 1.0, 1.0],
];

/// 加色混合（SrcAlpha/One；vireo 输出 straight alpha，fade/alpha 正常参与。
/// 不是 `BlendState::ADDITIVE`（One/One，给 premultiplied 输入用的）。
const ADDITIVE: BlendState = BlendState {
    color: BlendComponent {
        src_factor: BlendFactor::SrcAlpha,
        dst_factor: BlendFactor::One,
        operation: BlendOperation::Add,
    },
    alpha: BlendComponent {
        src_factor: BlendFactor::One,
        dst_factor: BlendFactor::One,
        operation: BlendOperation::Add,
    },
};

struct UiState {
    burst: bool,
    mode: u8,
    frozen: bool,
    material: bool,
    additive: bool,
    stress: bool,
    clear: bool,
    hud_text: bool,
    sustain_target: usize,
    present: bool,
}

fn spawn_burst(
    pool: &mut ParticlePool,
    rng: &mut u64,
    now: f32,
    cx: f32,
    cy: f32,
    mode: u8,
    n: u32,
) {
    if pool.live_count() > 120_000 {
        return;
    }
    for _ in 0..n {
        let angle = rand01(rng) * std::f32::consts::TAU;
        let (vel, life, half, color, quad, fade_in, fade_out) = match mode {
            1 => {
                // 环：统速度＋同寿命＋零淡入，干净 expanding circle
                let speed = 160.0;
                (
                    [angle.cos() * speed, angle.sin() * speed],
                    1.2,
                    4.0,
                    WHITE,
                    2,
                    0.0,
                    0.4,
                )
            }
            2 => {
                // 雨：下飘长寿小粒子
                (
                    [(rand01(rng) - 0.5) * 40.0, 120.0 + rand01(rng) * 100.0],
                    3.0 + rand01(rng) * 2.0,
                    2.0 + rand01(rng),
                    Color::new(0.6, 0.8, 1.0, 0.9),
                    0,
                    0.4,
                    1.0,
                )
            }
            _ => {
                // burst：随机方向/速度/寿命/颜色
                let speed = 60.0 + rand01(rng) * 200.0;
                let color = match (rand01(rng) * 3.0) as u32 {
                    0 => RED,
                    1 => YELLOW,
                    _ => WHITE,
                };
                (
                    [angle.cos() * speed, angle.sin() * speed],
                    0.8 + rand01(rng) * 0.8,
                    3.0 + rand01(rng) * 4.0,
                    color,
                    ((rand01(rng) * 4.0) as usize).min(3),
                    0.05,
                    0.5,
                )
            }
        };
        pool.spawn(Particle {
            pos: [cx, cy],
            vel,
            size: [half, half],
            color,
            uv_rect: QUADS[quad],
            birth: now,
            life,
            fade_in,
            fade_out,
            seed: rand01(rng),
        });
    }
}

const TINT_WGSL: &str = r#"
fn material_main(in: MaterialInput) -> vec4<f32> {
    let band = fract(in.local_pos.x * 0.02 + in.local_pos.y * 0.013);
    let tint = mix(vec3<f32>(1.0, 0.25, 0.1), vec3<f32>(0.2, 0.8, 1.0), band);
    return vec4<f32>(tint * in.color.rgb, in.color.a);
}
"#;

#[vireo::main]
async fn main(app: App) {
    let idx = app.window(WindowDesc::new("Particle Burst", 900, 700), None::<fn()>);
    let atlas = Texture::from_rgba(64, 64, &sprite_atlas(), &app.gpu);
    let tint = app
        .gpu
        .create_material(TINT_WGSL)
        .expect("particle tint material");

    let ui: Arc<Mutex<UiState>> = Arc::new(Mutex::new(UiState {
        burst: false,
        mode: 0,
        frozen: false,
        material: false,
        additive: false,
        stress: false,
        clear: false,
        hud_text: true,
        sustain_target: 0,
        present: false,
    }));
    let ui_keys = Arc::clone(&ui);
    app.on_key_down(idx, move |event| {
        if event.repeat {
            return;
        }
        let mut ui = ui_keys.lock().unwrap();
        match event.key {
            KeyCode::Space => ui.burst = true,
            KeyCode::Digit1 => ui.mode = 0,
            KeyCode::Digit2 => ui.mode = 1,
            KeyCode::Digit3 => ui.mode = 2,
            KeyCode::KeyT => ui.frozen = !ui.frozen,
            KeyCode::KeyM => ui.material = !ui.material,
            KeyCode::KeyB => ui.additive = !ui.additive,
            KeyCode::KeyG => ui.stress = true,
            KeyCode::KeyC => ui.clear = true,
            KeyCode::KeyH => ui.hud_text = !ui.hud_text,
            KeyCode::Equal => ui.sustain_target = (ui.sustain_target + 5000).min(100_000),
            KeyCode::Minus => ui.sustain_target = ui.sustain_target.saturating_sub(5000),
            KeyCode::KeyV => ui.present = true,
            _ => {}
        }
    });

    let mut pool = ParticlePool::new();
    let mut rng: u64 = 0x12345678;
    let mut ambient_done = false;
    let mut sustained: usize = 0;
    let mut present_immediate = false;
    // batch 跨帧复用（clear 保容量）：每帧 new 会把 particles/commands/vec 全打回小容量，
    // 18 万实例下 realloc churn 约 10ms（本地实测 fresh 12.9ms vs 复用 3.2ms）。
    let mut batch = DrawBatch::new();
    let mut sweep_tick: u32 = 0;
    let mut perf_line = String::new();
    let mut stats_line = String::new();
    // EMA 平滑＋降频刷新，否则每帧乱跳没法看
    let mut ema = [0.0f64; 4];
    let mut ema_init = false;
    let mut hud_tick: u32 = 0;

    app.run(move |ctx| {
        let win = match ctx.app().window_ref(&idx) {
            Ok(v) => v,
            Err(_) => return true,
        };
        // 冻结＝0 号钟停摆（scale 0），解冻＝恢复 1 倍速（从冻结点继续，无跳变）。
        // birth 全用同一时钟取值，前后一致。
        {
            let ui = ui.lock().unwrap();
            ctx.app()
                .gpu
                .set_clock_scale(ClockIndex::ZERO, if ui.frozen { 0.0 } else { 1.0 });
        }
        let now = ctx.app().gpu.clock_time(ClockIndex::ZERO);

        if !ambient_done {
            for _ in 0..120 {
                let x = rand01(&mut rng) * 900.0;
                let y = rand01(&mut rng) * 700.0;
                let angle = rand01(&mut rng) * std::f32::consts::TAU;
                let speed = 8.0 + rand01(&mut rng) * 20.0;
                pool.spawn(Particle {
                    pos: [x, y],
                    vel: [angle.cos() * speed, angle.sin() * speed],
                    size: [2.0, 2.0],
                    color: Color::new(0.5, 0.7, 1.0, 0.6),
                    uv_rect: QUADS[0],
                    birth: now,
                    life: -1.0,
                    fade_in: 1.0,
                    fade_out: 0.0,
                    seed: rand01(&mut rng),
                });
            }
            ambient_done = true;
        }

        {
            let mut ui = ui.lock().unwrap();
            if ui.clear {
                pool.clear();
                ambient_done = false;
                sustained = 0;
                ui.sustain_target = 0;
            }
            if ui.stress {
                // 压力档：30000 随机粒（life 4s 自 drain），同批仍 1 dc
                for _ in 0..30000 {
                    let x = rand01(&mut rng) * 900.0;
                    let y = rand01(&mut rng) * 700.0;
                    let angle = rand01(&mut rng) * std::f32::consts::TAU;
                    let speed = 20.0 + rand01(&mut rng) * 80.0;
                    pool.spawn(Particle {
                        pos: [x, y],
                        vel: [angle.cos() * speed, angle.sin() * speed],
                        size: [2.0, 2.0],
                        color: WHITE,
                        uv_rect: QUADS[0],
                        birth: now,
                        life: 4.0,
                        fade_in: 0.2,
                        fade_out: 1.0,
                        seed: rand01(&mut rng),
                    });
                }
            }
            if ui.burst {
                let (mx, my) = win.mouse_pos().logical();
                spawn_burst(&mut pool, &mut rng, now, mx as f32, my as f32, ui.mode, 60);
            }
            ui.burst = false;
            ui.stress = false;
            ui.clear = false;
            // 持续负载：immortal 漂移顶到目标数（稳态 N，供长时间测帧率）
            let target = ui.sustain_target;
            while sustained < target {
                let x = rand01(&mut rng) * 900.0;
                let y = rand01(&mut rng) * 700.0;
                let angle = rand01(&mut rng) * std::f32::consts::TAU;
                let speed = 8.0 + rand01(&mut rng) * 20.0;
                pool.spawn(Particle {
                    pos: [x, y],
                    vel: [angle.cos() * speed, angle.sin() * speed],
                    size: [2.0, 2.0],
                    color: WHITE,
                    uv_rect: QUADS[0],
                    birth: now,
                    life: -1.0,
                    fade_in: 0.0,
                    fade_out: 0.0,
                    seed: rand01(&mut rng),
                });
                sustained += 1;
            }
        }

        // 过期 kill：sweep 每 30 帧一次（0.57ms→均摊 0.02ms）。
        // 到期未扫的槽位照推照传，VS 按 age 剔除不可见——sweep 只是内存回收，
        // 不影响画面正确性。生产池可按死亡率自适应间隔。
        sweep_tick += 1;
        if sweep_tick % 30 == 0 {
            pool.sweep(now);
        }

        let material_on = ui.lock().unwrap().material;
        let mode = ui.lock().unwrap().mode;
        let frozen = ui.lock().unwrap().frozen;
        let hud_on = ui.lock().unwrap().hud_text;
        let target = ui.lock().unwrap().sustain_target;
        if std::mem::take(&mut ui.lock().unwrap().present) {
            present_immediate = !present_immediate;
            win.set_present_mode(if present_immediate {
                PresentMode::Immediate
            } else {
                PresentMode::AutoVsync
            });
        }
        // 复用外层 batch（clear 保容量）；首帧外层为空，与 new 等价
        batch.clear();
        batch.set_texture(Some(&atlas));
        if material_on {
            batch.set_custom_material(Some(tint.clone()));
        }
        if ui.lock().unwrap().additive {
            batch.set_blend_state(ADDITIVE);
        }
        let t0 = std::time::Instant::now();
        draw_particles(&mut batch, &pool);
        let build_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let mode_name = ["burst", "ring", "rain"][mode as usize];
        if hud_on {
            draw_text(
                &mut batch.texts,
                &stats_line,
                Pos::new(10., 10.),
                TextDef::default(),
                TextOverride::new(),
            );
            draw_text(
                &mut batch.texts,
                "Space=emit 1/2/3=mode T=freeze M=material B=additive G=stress30k +/-=sustain C=clear H=text V=present",
                Pos::new(10., 34.),
                TextDef::default(),
                TextOverride::new(),
            );
            draw_text(
                &mut batch.texts,
                &perf_line,
                Pos::new(10., 58.),
                TextDef::default(),
                TextOverride::new(),
            );
        }

        let report = win.draw(Color::new(0.03, 0.03, 0.06, 1.0), &[&batch]);
        let sample = [
            build_ms,
            report.timings.encode_secs * 1000.0,
            report.timings.acquire_secs * 1000.0,
            report.timings.present_secs * 1000.0,
        ];
        if !ema_init {
            ema = sample;
            ema_init = true;
        } else {
            for i in 0..4 {
                ema[i] += (sample[i] - ema[i]) * 0.08;
            }
        }
        hud_tick += 1;
        if hud_tick % 15 == 0 {
            stats_line = format!(
                "live={} target={} fps={:.0} dc={} t={:.1}{} mode={} material={} blend={} {}",
                pool.live_count(),
                target,
                ctx.fps(),
                win.last_draw_calls(),
                now,
                if frozen { " FROZEN" } else { "" },
                mode_name,
                if material_on { "on" } else { "off" },
                if ui.lock().unwrap().additive {
                    "add"
                } else {
                    "alpha"
                },
                if present_immediate { " IMM" } else { "" },
            );
            perf_line = format!(
                "build={:.2}ms enc={:.2}ms acq={:.2}ms pres={:.2}ms",
                ema[0], ema[1], ema[2], ema[3],
            );
        }
        true
    })
    .await
    .unwrap();
}
