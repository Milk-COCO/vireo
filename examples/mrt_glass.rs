//! MRT 的**用途**示例：用掩码通道做一块磨砂玻璃面板。
//!
//! 只要"磨砂"这一层：掩码 + 分离式高斯。想要折射/色散/会动的镜面，看
//! `mrt_liquid_glass.rs`（它在这套管线之上多加了一个"距边距离"通道）。
//!
//! 四个 pass：
//!
//! 1. **MRT pass**（几何只画一遍，两个材质）
//!    - 背景那批用材质 A：写 albedo，`mask = 0`；
//!    - 玻璃面板用材质 B：**不写颜色**（alpha = 0），只把 `mask` 写成 1。
//!      于是 target 0 里只有背景，target 1 里只有"面板覆盖区域"。
//! 2. **降采样**：把 `scene` 画进 1/4 分辨率的 `q0`（线性采样自带 2×2 平均）。
//! 3. **分离式高斯**：`q0 → q1` 横一遍、`q1 → q0` 竖一遍（各 5 tap，线性采样等效 9 tap）。
//! 4. **合成**：`scene`（清晰）+ `q0`（模糊）+ `mask` ——
//!    - 模糊与清晰按掩码混合，只糊面板区；
//!    - 采样点沿**掩码梯度**偏移 → 边缘处背景被吸进来，形成玻璃折射；
//!    - `|∇mask|` 当切边高光；掩码向上偏移取样当面板投影。
//!
//! 为什么不用"一圈环形采样"代替模糊：稀疏环形采样在**高频背景**上会混叠（采样盘
//! 显形，局部出现"空洞比实心还亮"的假象）。分离式高斯在 1/4 分辨率上做，代价低、
//! 结果稳定，跟背景内容无关。
//!
//! 为什么需要 MRT：面板覆盖区域是"形状的几何信号"，不是颜色。没有它，磨砂只能靠
//! **把背景再画一遍**（而且要画在面板坐标系里），或者用一个矩形近似硬裁。
//!
//! ## 几个容易踩的点
//!
//! - 只有 **custom material** 能写多目标；引擎内置形状/文字在附件数 ≠ 1 时会被跳过。
//! - 面板材质**不能**用全写（`blend: None`）画进 target 0，否则会把背景擦掉；
//!   这里让它输出 `alpha = 0`、走正常的 alpha 混合，等于"只写掩码不写颜色"。
//! - 附加目标固定 `LoadOp::Load` 不清屏：背景那批负责把 `mask = 0` 铺满全屏。
//! - 采样偏移按**像素**算，不能直接用 uv（宽高不等时偏移会各向异性）。
//! - 分离式高斯的横/竖两遍用**同一个材质 + 一个方向 uniform**；因为两遍各自独立
//!   submit，`write_buffer` 的先后是有保证的（同一次提交内改 uniform 就不成立）。

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

/// 玻璃面板：不写颜色（alpha = 0，等于对背景无副作用），只写掩码。
///
/// 掩码要写**覆盖度**而不是 `1.0`：引擎只对 `color` 应用 SDF 羽化，其余输出原样写出，
/// 直接写 1 会得到二值硬边——面板边界出现踏步，而场景里形状的边缘是抗锯齿的，
/// 两者叠一起就是"又锯齿又抗锯齿"。
const PANEL_FRAG: &str = r#"
struct MaterialOutput {
    @location(0) color: vec4<f32>,
    @location(1) mask: vec4<f32>,
};

// 圆角矩形（sdf_type == 2）的有符号距离，单位与 local_pos 一致（逻辑像素）
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
    let cover = 1.0 - smoothstep(0.0, feather, sd);
    let edge_t = clamp(-sd / 26.0, 0.0, 1.0);   // 0 = 边缘，1 = 内部（只给合成阶段定方向用）
    var out: MaterialOutput;
    out.color = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    out.mask = vec4<f32>(cover, edge_t, 0.0, 1.0);
    return out;
}
"#;

/// 分离式高斯的一遍：`dir` 为 (1,0) 或 (0,1)。
/// 5 个采样点经线性过滤等效 9 tap，权重和恰为 1。
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

/// 合成：清晰 / 模糊 / 掩码三张图。
const GLASS_FRAG: &str = r#"
fn material_main(in: MaterialInput) -> vec4<f32> {
    let uv = in.uv;
    let px = vec2<f32>(1.0 / 760.0, 1.0 / 420.0);
    let m = textureSample(mask, mask_samp, uv).r;

    // 掩码梯度 → 玻璃边缘的"厚度方向"。**幅值**取自覆盖度的轴向差分（保持原样），
    // **方向**改由距边距离场 t 的导数决定：轴向差分在圆角处会把方向量化成阶梯
    // （x/y 两分量不同步），t 是连续场，其梯度方向在曲线上平滑旋转。
    let gx = textureSample(mask, mask_samp, uv + vec2<f32>(4.0 * px.x, 0.0)).r
           - textureSample(mask, mask_samp, uv - vec2<f32>(4.0 * px.x, 0.0)).r;
    let gy = textureSample(mask, mask_samp, uv + vec2<f32>(0.0, 4.0 * px.y)).r
           - textureSample(mask, mask_samp, uv - vec2<f32>(0.0, 4.0 * px.y)).r;
    let gp = vec2<f32>(gx, gy);
    let t = textureSample(mask, mask_samp, uv).g;
    let dt = vec2<f32>(dpdx(t), dpdy(t));
    let dt_len = length(dt);
    let g = select(gp, dt / max(dt_len, 1e-6) * length(gp), dt_len > 1e-5);

    // 折射：沿边缘梯度把背景吸进来（中间平坦，只在边上弯）
    let refr = g * 22.0 * px;

    // 磨砂结果来自 1/4 分辨率的两遍高斯，线性放回全分辨率
    let blur = textureSample(blurred, blurred_samp, uv + refr).rgb;
    let sharp = textureSample(scene, scene_samp, uv).rgb;

    // 玻璃本体：压一点对比 + 冷色染色 + 提亮
    let glass = blur * 0.90 + vec3<f32>(0.05, 0.08, 0.115);
    var col = mix(sharp, glass, m);

    // 切边高光
    let rim = clamp(length(g) * 1.8, 0.0, 1.0) * m;
    col += vec3<f32>(0.85, 0.93, 1.00) * rim * 0.30;

    // 面板投影：向上取样（光源在上方）→ 只有面板下方变暗
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
    let window = app.window(WindowDesc::new("MRT: frosted glass", W, H), None::<fn()>);
    let scene = app.offscreen(W, H, AntiAliasing::None);
    let mask = app.offscreen(W, H, AntiAliasing::None);
    // 1/4 分辨率的两张缓冲：横竖两遍交替读写，避免读自己
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
            MaterialResources(&[tex_slot("scene"), tex_slot("blurred"), tex_slot("mask")]),
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
        // 高频细节：分离式高斯不会因它混叠（旧的稀疏环形采样会），
        // 所以可以放心用——面板内一糊就明显"磨砂"了。
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

        // ---- Pass 1（同一 pass 的第二个材质）：只写掩码 ----
        let mut glass_panel = DrawBatch::new();
        glass_panel.set_custom_material(Some(panel.clone()));
        draw_rounded_rect(
            &mut glass_panel,
            Pos::new(240.0, 60.0),
            320.0,
            300.0,
            34.0,
            Some(WHITE),
        );

        let mask_target = ColorTargetDesc {
            view: mask.texture.lock().view.clone(),
            resolve_target: None,
            blend: None,
        };
        scene.draw_mrt(Some(BLACK), &[mask_target], &[&world, &glass_panel]);

        // ---- Pass 2：降采样到 1/4（线性采样自带 2x2 平均）----
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

        // ---- Pass 3/4：分离式高斯（横、竖各一遍）----
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
        // 两遍之后模糊结果在 q0

        // ---- Pass 5：合成 ----
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
            "MRT: frosted glass (separable blur)",
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
