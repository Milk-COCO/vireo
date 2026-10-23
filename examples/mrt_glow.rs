//! MRT 的**用途**示例：用第二路通道做出没有它就做不出来的效果（光晕）。
//!
//! 只想看 MRT 本身怎么写，看 `mrt.rs`（一次 pass 写两张图，摊开对比）。本示例是它的
//! 下游：把第二张图真正**用起来**。
//!
//! 两个 pass：
//!
//! 1. **MRT pass**（几何只画一遍）：片元着色器同时写两张图
//!    - target 0 `scene`：场景颜色；
//!    - target 1 `mask`：选中掩码（这里只把圆形标 1）。
//! 2. **合成 pass**：一个自定义材质**同时采样这两张图**——对 `mask` 做环形采样近似
//!    模糊，取 `blur(mask) - mask` 作为**形状外侧的光晕**，加到 `scene` 上。
//!
//! 为什么非 MRT 不可：
//!
//! - 光晕需要"形状的覆盖区域"作为独立信号。没有它就只能把几何**再画一遍**（画大一圈、
//!   带透明度）来伪造光晕——形状一多、一变形，两次绘制就得逐帧对齐，成本翻倍还容易错。
//! - 而 `blur(mask) - mask` 这种"取边缘外侧"的表达式，必须有"覆盖区域"这一路数据才写得出来。
//! - 掩码和颜色由同一批片元产出，天生逐像素对齐 —— 这是 MRT 的根本价值。
//!
//! 场景里方块是**对照组**：同样的几何、同样的绘制，只是不在掩码里，所以一点光都没有。
//! 光晕的差别完全来自第二路数据，不是颜色或形状。
//!
//! ## 几个容易踩的点
//!
//! - 只有 **custom material** 能写多目标：fragment 返回 `MaterialOutput`，
//!   **第 0 个字段必须叫 `color`**（引擎在它上面应用 alpha / SDF），其余 location 原样写出。
//! - 附加目标固定 `LoadOp::Load`，**不会自动清屏**：掩码要么全写（`blend: None`，本帧每像素
//!   覆盖一次），要么自己先清一遍；**不能**用加性混合凑并集——形状一动就会累加出运动残影。
//! - 掩码是**硬边**的：引擎只对 `color` 应用 SDF 羽化，其余输出原样返回。所以光晕内侧会有
//!   约 1px 的硬边界；要更干净就把覆盖度也乘进掩码（见 `vireo_apply_sdf` 的用法）。
//! - 采样偏移要按**像素**算：uv 是 0..1 归一化的，宽高不等时同一个 uv 偏移在横纵方向
//!   对应的像素数不同，直接用 uv 偏移会得到横向拉长的光晕。
//! - 颜色是**线性值**：目标格式是 sRGB 时写入会被编码，所以深底用 `0.005` 级而不是 `0.06`。

use vireo::prelude::*;

const W: u32 = 760;
const H: u32 = 420;

/// Pass 1：一次写两个目标。
const GEOM_FRAG: &str = r#"
struct MaterialOutput {
    @location(0) color: vec4<f32>,
    @location(1) mask: vec4<f32>,
};

fn material_main(in: MaterialInput) -> MaterialOutput {
    let base = vireo_base_sample(in.uv);
    let sel = select(0.0, 1.0, in.sdf_type == 1u);   // 只选圆形
    var out: MaterialOutput;
    out.color = base * in.color;
    out.mask = vec4<f32>(sel, sel, sel, 1.0);
    return out;
}
"#;

/// Pass 2：同时采样 `scene` 与 `mask`，用掩码的"外侧"部分叠发光。
const COMPOSITE_FRAG: &str = r#"
fn material_main(in: MaterialInput) -> vec4<f32> {
    let uv = in.uv;
    let base_col = textureSample(scene, scene_samp, uv).rgb;
    let core = textureSample(mask, mask_samp, uv).r;

    // 两圈、每圈 12 个方向的环形采样 ≈ 一次廉价模糊。
    // 光晕 = blur(mask) - mask：只有形状**外侧**为正，内侧自动归零。
    // 偏移必须按像素算：uv 是 0..1 归一化的，760x420 的图上同一个 uv 偏移
    // 横向和纵向的像素数不同（直接用 uv 偏移会得到横向拉长的光晕）。
    let px = vec2<f32>(1.0 / 760.0, 1.0 / 420.0);
    var ring = 0.0;
    for (var i = 0; i < 16; i++) {
        let a = f32(i) * 0.3926991;      // 22.5°
        let d = vec2<f32>(cos(a), sin(a));
        ring += textureSample(mask, mask_samp, uv + d * px * 9.0).r * 0.40;
        ring += textureSample(mask, mask_samp, uv + d * px * 22.0).r * 0.28;
        ring += textureSample(mask, mask_samp, uv + d * px * 38.0).r * 0.20;
        ring += textureSample(mask, mask_samp, uv + d * px * 56.0).r * 0.12;
    }
    ring = ring / 16.0;
    let halo = max(ring - core, 0.0);

    // 场景稍压暗，让光晕更明显
    return vec4<f32>(base_col * 0.85 + vec3<f32>(0.05, 0.45, 0.75) * halo * 1.8, 1.0);
}
"#;

#[vireo::main]
async fn main() {
    let window = app.window(
        WindowDesc::new("MRT glow: mask-driven halo", W, H),
        None::<fn()>,
    );
    let scene = app.offscreen(W, H, AntiAliasing::None);
    let mask = app.offscreen(W, H, AntiAliasing::None);

    let geom = app.material(GEOM_FRAG).expect("geom material");
    let composite = app
        .material_with_resources(
            COMPOSITE_FRAG,
            MaterialResources(&[
                MaterialResource {
                    name: "scene",
                    kind: MaterialResourceKind::Texture {
                        view: TexKind::D2(TexSample::Float),
                        sampler: SampKind::Filtering,
                    },
                },
                MaterialResource {
                    name: "mask",
                    kind: MaterialResourceKind::Texture {
                        view: TexKind::D2(TexSample::Float),
                        sampler: SampKind::Filtering,
                    },
                },
            ]),
        )
        .expect("composite material");

    let device = app.gpu().device.clone();
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("mrt example sampler"),
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
        t += 0.016;

        // ---- Pass 1：一次 MRT 提交，几何只画一遍 ----
        let mut geo = DrawBatch::new();
        geo.set_custom_material(Some(geom.clone()));
        draw_rectangle(
            &mut geo,
            Pos::new(0.0, 0.0),
            W as f32,
            H as f32,
            Some(Color::new(0.005, 0.006, 0.013, 1.0)),
        );
        for i in 0..4 {
            let x = 120.0 + i as f32 * 175.0;
            let y = 210.0 + (t * 1.8 + i as f32 * 0.8).sin() * 70.0;
            if i % 2 == 0 {
                // 偶数位画圆 —— 它会出现在掩码里，因此会发光
                draw_circle(&mut geo, Pos::new(x, y), 46.0, Some(ORANGE));
            } else {
                // 奇数位画方块 —— 掩码为 0，不发光
                draw_rectangle(
                    &mut geo,
                    Pos::new(x - 42.0, y - 42.0),
                    84.0,
                    84.0,
                    Some(MAGENTA),
                );
            }
        }
        let mask_target = ColorTargetDesc {
            view: mask.texture.lock().view.clone(),
            resolve_target: None,
            blend: None,
        };
        scene.draw_mrt(Some(BLACK), &[mask_target], &[&geo]);

        // ---- Pass 2：采样两张图合成（只有圆形带光晕）----
        {
            let scene = scene.texture.lock();
            let mask = mask.texture.lock();
            composite.set_texture(&device, "scene", &scene.view, &sampler);
            composite.set_texture(&device, "mask", &mask.view, &sampler);
        }
        let mut blit = DrawBatch::new();
        blit.set_custom_material(Some(composite.clone()));
        draw_rectangle(
            &mut blit,
            Pos::new(0.0, 0.0),
            W as f32,
            H as f32,
            Some(WHITE),
        );
        win.draw(BLACK, &[&blit]);
        true
    })
    .await
    .unwrap();
}
