//! MRT 的**用途**示例：用掩码通道做一块"液态玻璃"面板（Liquid Glass 风格）。
//!
//! 这是磨砂玻璃（`mrt_glass.rs`）的增强版：同一套掩码 + 分离式高斯管线，但掩码多带了
//! 一个**距边距离**，于是能做透镜折射、色散和会动的镜面。
//!
//! 四个 pass：
//!
//! 1. **MRT pass**（几何只画一遍，两个材质）
//!    - 背景那批用材质 A：写 albedo，`mask.xy = (0, 0)`；
//!    - 玻璃面板用材质 B：**不写颜色**（alpha = 0），只写掩码——
//!      `mask.r = 覆盖度`（抗锯齿）、`mask.g = 距边距离 / 倒角宽度`（0 边缘 → 1 内部）。
//! 2. **降采样**：`scene` → 1/4 分辨率的 `q0`（线性采样自带 2×2 平均）。
//! 3. **分离式高斯**：`q0 → q1` 横、`q1 → q0` 竖（各 5 tap，线性等效 9 tap）。
//! 4. **合成**：清晰 / 模糊 / 掩码（含距边距离）——
//!    - **边缘透镜**：位移剖面用 `sin(π·t)`，中段最强、边缘与内部归零，等于把背景在
//!      倒角带里"折"一下；单纯按梯度平移只会得到整体错位，不像厚玻璃；
//!    - **色散**：R/G/B 的位移量略有差异（×0.94 / ×1.00 / ×1.06），边缘出现彩边；
//!    - **镜面**：朝向鼠标的一侧最亮，`exp(-k·t)` 让它只贴着边；
//!    - **厚度暗带**：`1 - 0.25·exp(-9t)`，边缘微微压暗，玻璃才有"厚度"；
//!    - 掩码偏移取样做投影。
//!
//! 为什么需要 MRT：以上全部依赖"距边距离"和"覆盖度"这两个**形状几何信号**。
//! 没有多目标，要么把几何再画一遍到另一张纹理去写距离场，要么只能用矩形近似硬裁。
//!
//! ## 几个容易踩的点
//!
//! - 只有 **custom material** 能写多目标；引擎内置形状/文字在附件数 ≠ 1 时会被跳过。
//! - 面板材质**不能**用全写（`blend: None`）画进 target 0，否则会把背景擦掉；
//!   这里让它输出 `alpha = 0`、走正常 alpha 混合，等于"只写掩码不写颜色"。
//! - 掩码要写**覆盖度**而不是 `1.0`：引擎只对 `color` 应用 SDF 羽化，其余输出原样写出。
//!   直接写 1 会得到二值硬边，面板边界出现踏步（"又锯齿又抗锯齿"就是这么来的）。
//! - 附加目标固定 `LoadOp::Load` 不清屏：背景那批负责把 `mask = 0` 铺满全屏。
//! - 位移量按**像素**算再换算成 uv；直接用 uv 偏移会在宽高不等时各向异性。

use vireo::prelude::*;

const W: u32 = 760;
const H: u32 = 420;
/// 模糊缓冲的分辨率（1/4）。
const QW: u32 = W / 4;
const QH: u32 = H / 4;

/// 背景：写颜色，掩码恒 0。
const BG_FRAG: &str = r#"
struct MaterialOutput {
    @location(0) color: vec4<f32>,
    @location(1) mask: vec4<f32>,
};

fn material_main(in: MaterialInput) -> MaterialOutput {
    let base = vireo_base_sample(in.uv);
    var out: MaterialOutput;
    out.color = base * in.color;
    out.mask = vec4<f32>(0.0, 0.0, 0.0, 1.0);
    return out;
}
"#;

/// 玻璃面板：不写颜色，只写"覆盖度 + 距边距离"。
const PANEL_FRAG: &str = r#"
struct MaterialOutput {
    @location(0) color: vec4<f32>,
    @location(1) mask: vec4<f32>,
};

// 圆角矩形（sdf_type == 2）的有符号距离，单位同 local_pos（逻辑像素，内侧为负）
fn rect_sd(in: MaterialInput) -> f32 {
    let p = in.local_pos - in.sdf_params.xy;
    let half = in.sdf_params.zw;
    let r = in.sdf_extra.x;
    let q = abs(p) - (half - vec2<f32>(r));
    return length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - r;
}

fn material_main(in: MaterialInput) -> MaterialOutput {
    let sd = rect_sd(in);
    let feather = max(in.sdf_feather, 1.0);
    let cover = 1.0 - smoothstep(0.0, feather, sd);   // 抗锯齿的覆盖度
    let edge_t = clamp(-sd / 26.0, 0.0, 1.0);         // 0 = 边缘，1 = 内部（倒角带 26px）
    var out: MaterialOutput;
    out.color = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    out.mask = vec4<f32>(cover, edge_t, 0.0, 1.0);
    return out;
}
"#;

/// 分离式高斯的一遍：`dir` 为 (1,0) 或 (0,1)。
const BLUR_FRAG: &str = r#"
fn material_main(in: MaterialInput) -> vec4<f32> {
    let uv = in.uv;
    let texel = vec2<f32>(1.0 / 190.0, 1.0 / 105.0);   // = QW/QH
    let d = dir.xy * texel;

    var c = textureSample(src, src_samp, uv).rgb * 0.227027;
    c += textureSample(src, src_samp, uv + d * 1.384615).rgb * 0.316216;
    c += textureSample(src, src_samp, uv - d * 1.384615).rgb * 0.316216;
    c += textureSample(src, src_samp, uv + d * 3.230769).rgb * 0.070270;
    c += textureSample(src, src_samp, uv - d * 3.230769).rgb * 0.070270;
    return vec4<f32>(c, 1.0);
}
"#;

/// 合成：透镜 + 色散 + 镜面 + 投影。
const GLASS_FRAG: &str = r#"
fn material_main(in: MaterialInput) -> vec4<f32> {
    let uv = in.uv;
    let px = vec2<f32>(1.0 / 760.0, 1.0 / 420.0);
    let m = textureSample(mask, mask_samp, uv).r;      // 覆盖度（抗锯齿）
    let t = textureSample(mask, mask_samp, uv).g;      // 0 = 边缘，1 = 内部

    // 边缘方向：幅值沿用覆盖度的轴向差分（保持原样），方向改由距边距离场 t 的导数决定。
    // 轴向差分在圆角处会把方向量化成阶梯（x/y 分量不同步），t 是连续场，梯度方向平滑旋转。
    let gx = textureSample(mask, mask_samp, uv + vec2<f32>(4.0 * px.x, 0.0)).r
           - textureSample(mask, mask_samp, uv - vec2<f32>(4.0 * px.x, 0.0)).r;
    let gy = textureSample(mask, mask_samp, uv + vec2<f32>(0.0, 4.0 * px.y)).r
           - textureSample(mask, mask_samp, uv - vec2<f32>(0.0, 4.0 * px.y)).r;
    let gp = normalize(vec2<f32>(gx / px.x, gy / px.y) + vec2<f32>(1e-5));
    let dt = vec2<f32>(dpdx(t), dpdy(t));
    let dt_len = length(dt);
    let dir = select(gp, dt / max(dt_len, 1e-6), dt_len > 1e-5);

    // 透镜：sin(π·t) 让位移在倒角带中段最大、两端归零 —— 这才是"厚玻璃折一下"，
    // 单纯沿梯度平移只会让整块内容错位。
    let band = sin(clamp(t, 0.0, 1.0) * 3.14159265);
    let off = dir * (18.0 * band) * px;

    // 色散：三个通道的位移量略不同，边缘出现彩边
    let lensed = vec3<f32>(
        textureSample(scene, scene_samp, uv + off * 0.94).r,
        textureSample(scene, scene_samp, uv + off * 1.00).g,
        textureSample(scene, scene_samp, uv + off * 1.06).b,
    );
    let frosted = textureSample(blurred, blurred_samp, uv + off).rgb;
    var glass = mix(lensed, frosted, 0.5);

    // 通透感：提饱和 + 轻微冷色 + 边缘厚度暗带
    let lum = dot(glass, vec3<f32>(0.299, 0.587, 0.114));
    glass = mix(vec3<f32>(lum), glass, 1.18) * vec3<f32>(0.97, 1.00, 1.05);
    glass *= 1.0 - 0.25 * exp(-t * 9.0);
    glass += vec3<f32>(0.015, 0.022, 0.035);

    let sharp = textureSample(scene, scene_samp, uv).rgb;
    var col = mix(sharp, glass, m);

    // 镜面：朝向鼠标的一侧最亮，只贴着边
    let to_l = light_px.xy - uv * vec2<f32>(760.0, 420.0);
    let ld = normalize(to_l + vec2<f32>(1e-4));
    let spec = pow(max(dot(dir, ld), 0.0), 3.0) * exp(-t * 5.0) * m;
    col += vec3<f32>(0.90, 0.95, 1.00) * spec * 1.5;
    col += vec3<f32>(0.70, 0.80, 0.95) * exp(-t * 7.0) * m * 0.12;

    // 投影：向上取样（光源在上方）→ 只有面板下方变暗
    var sh = 0.0;
    for (var i = 1; i <= 6; i++) {
        let o = vec2<f32>(0.0, -f32(i) * 4.0 * px.y);
        sh += textureSample(mask, mask_samp, uv + o).r * (1.0 - f32(i) / 7.0);
    }
    col *= 1.0 - clamp(sh, 0.0, 1.0) * 0.55 * (1.0 - m);

    return vec4<f32>(col, 1.0);
}
"#;

fn tex_slot(name: &'static str) -> MaterialResource<'static> {
    MaterialResource {
        name,
        kind: MaterialResourceKind::Texture {
            view: TexKind::D2(TexSample::Float),
            sampler: SampKind::Filtering,
        },
    }
}

#[vireo::main]
async fn main() {
    let window = app.window(WindowDesc::new("MRT: liquid glass", W, H), None::<fn()>);
    let scene = app.offscreen(W, H, AntiAliasing::None);
    let mask = app.offscreen(W, H, AntiAliasing::None);
    let q0 = app.offscreen(QW, QH, AntiAliasing::None);
    let q1 = app.offscreen(QW, QH, AntiAliasing::None);

    let bg = app.material(BG_FRAG).expect("bg material");
    let panel = app.material(PANEL_FRAG).expect("panel material");
    let blur = app
        .material_with_resources(
            BLUR_FRAG,
            MaterialResources(&[
                tex_slot("src"),
                MaterialResource {
                    name: "dir",
                    kind: MaterialResourceKind::Uniform {
                        min_size: 16,
                        type_name: "vec4<f32>",
                        dynamic: false,
                    },
                },
            ]),
        )
        .expect("blur material");
    let glass = app
        .material_with_resources(
            GLASS_FRAG,
            MaterialResources(&[
                tex_slot("scene"),
                tex_slot("blurred"),
                tex_slot("mask"),
                MaterialResource {
                    name: "light_px",
                    kind: MaterialResourceKind::Uniform {
                        min_size: 16,
                        type_name: "vec4<f32>",
                        dynamic: false,
                    },
                },
            ]),
        )
        .expect("glass material");

    let device = app.gpu().device.clone();
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("mrt_glass sampler"),
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        ..Default::default()
    });

    let mut t: f32 = 0.0;

    app.run(move |ctx| {
        let win = match ctx.app().window_ref(&window) {
            Ok(v) => v,
            Err(_) => return false,
        };
        let scene = match ctx.app().offscreen_ref(&scene) {
            Ok(v) => v,
            Err(_) => return false,
        };
        let mask = match ctx.app().offscreen_ref(&mask) {
            Ok(v) => v,
            Err(_) => return false,
        };
        let q0 = match ctx.app().offscreen_ref(&q0) {
            Ok(v) => v,
            Err(_) => return false,
        };
        let q1 = match ctx.app().offscreen_ref(&q1) {
            Ok(v) => v,
            Err(_) => return false,
        };
        t += 0.016;

        // ---- Pass 1：背景（写颜色 + mask=0）----
        let mut world = DrawBatch::new();
        world.set_custom_material(Some(bg.clone()));
        draw_rectangle(
            &mut world,
            Pos::new(0.0, 0.0),
            W as f32,
            H as f32,
            Some(Color::new(0.012, 0.016, 0.035, 1.0)),
        );
        for (y, c) in [(88.0, SKYBLUE), (190.0, ORANGE), (292.0, PINK)] {
            draw_rounded_rect(
                &mut world,
                Pos::new(-20.0, y + (t * 0.9).sin() * 6.0),
                W as f32 + 40.0,
                46.0,
                23.0,
                Some(c),
            );
        }
        // 高频细节：分离式高斯不会因它混叠
        for gy in 0..11 {
            for gx in 0..20 {
                draw_circle(
                    &mut world,
                    Pos::new(24.0 + gx as f32 * 38.0, 26.0 + gy as f32 * 38.0),
                    4.0,
                    Some(Color::new(0.34, 0.40, 0.55, 1.0)),
                );
            }
        }
        draw_circle(&mut world, Pos::new(150.0, 150.0), 92.0, Some(GOLD));
        draw_circle(&mut world, Pos::new(640.0, 320.0), 104.0, Some(PURPLE));

        // ---- Pass 1（同一 pass 的第二个材质）：面板只写掩码 ----
        let mut panel_batch = DrawBatch::new();
        panel_batch.set_custom_material(Some(panel.clone()));
        draw_rounded_rect(
            &mut panel_batch,
            Pos::new(180.0, 70.0),
            400.0,
            280.0,
            56.0,
            Some(WHITE),
        );

        let mask_target = ColorTargetDesc {
            view: mask.texture.lock().view.clone(),
            resolve_target: None,
            blend: None,
        };
        scene.draw_mrt(Some(BLACK), &[mask_target], &[&world, &panel_batch]);

        // ---- Pass 2：降采样到 1/4 ----
        {
            let mut down = DrawBatch::new();
            let s = scene.texture.lock();
            down.set_texture(Some(&*s));
            draw_rectangle(
                &mut down,
                Pos::new(0.0, 0.0),
                QW as f32,
                QH as f32,
                Some(WHITE),
            );
            q0.draw(Some(BLACK), &[&down]);
        }

        // ---- Pass 3/4：分离式高斯 ----
        for (dx, dy) in [(1.0f32, 0.0f32), (0.0f32, 1.0f32)] {
            blur.set_uniform(&ctx.app().gpu().queue, "dir", &[dx, dy, 0.0, 0.0]);
            let (src, dst) = if dx > 0.5 { (&q0, &q1) } else { (&q1, &q0) };
            {
                let tex = src.texture.lock();
                blur.set_texture(&device, "src", &tex.view, &sampler);
            }
            let mut b = DrawBatch::new();
            b.set_custom_material(Some(blur.clone()));
            draw_rectangle(
                &mut b,
                Pos::new(0.0, 0.0),
                QW as f32,
                QH as f32,
                Some(WHITE),
            );
            dst.draw(Some(BLACK), &[&b]);
        }

        // ---- Pass 5：合成 ----
        // 镜面光源跟着鼠标；鼠标没进来时绕一圈
        let (mx, my) = win.mouse_pos().logical();
        let (lx, ly): (f32, f32) = if mx < 0.0 || my < 0.0 {
            (
                W as f32 * 0.5 + (t * 1.1).cos() * 300.0,
                H as f32 * 0.5 + (t * 1.1).sin() * 180.0,
            )
        } else {
            (mx as f32, my as f32)
        };
        glass.set_uniform(&ctx.app().gpu().queue, "light_px", &[lx, ly, 0.0, 0.0]);

        {
            let s = scene.texture.lock();
            let b = q0.texture.lock();
            let m = mask.texture.lock();
            glass.set_texture(&device, "scene", &s.view, &sampler);
            glass.set_texture(&device, "blurred", &b.view, &sampler);
            glass.set_texture(&device, "mask", &m.view, &sampler);
        }
        let mut blit = DrawBatch::new();
        blit.set_custom_material(Some(glass.clone()));
        draw_rectangle(
            &mut blit,
            Pos::new(0.0, 0.0),
            W as f32,
            H as f32,
            Some(WHITE),
        );

        let mut hud = DrawBatch::new();
        draw_text(
            &mut hud.texts,
            "MRT: liquid glass - move the mouse",
            Pos::new(12.0, 10.0),
            TextDef::default().font_size(13.0),
            TextOverride::from_color(Color::new(0.72, 0.78, 0.9, 1.0)),
        );

        win.draw(BLACK, &[&blit, &hud]);
        true
    })
    .await
    .unwrap();
}
