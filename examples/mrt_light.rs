//! MRT 的**用途**示例：albedo + 法线两路数据 → 把法线当**样式场**用。
//!
//! 这是全套 MRT 示例里唯一写"连续向量场"的：其它几个的第二路都是掩码/标量（覆盖度、
//! 距边距离），只有这里每个通道都有方向含义、且**形状内部也在变**。
//!
//! 两个 pass：
//!
//! 1. **MRT pass**（几何只画一遍）
//!    - target 0 `scene`：albedo；
//!    - target 1 `normal`：法线（编码成 `n*0.5+0.5`）。形状是解析的，法线几乎免费：
//!      取**有符号距离的梯度**（`dpdx/dpdy`），整形成柔和穹顶。
//! 2. **样式 pass**：法线不做光照，只做两件事——
//!    **斜向柔光带**（低对比渐变，像 mesh gradient）和**极窄内发光**（跟着鼠标那一侧更亮），
//!    外加接触阴影。刻意不用 Lambert / Blinn-Phong / 环境压暗：那套"一盏灯照在模型上"
//!    的配方放在扁平 UI 上会立刻显得像上个世纪的皮肤。
//!
//! ## 为什么这属于 MRT 的典型收益
//!
//! 法线是"形状的几何信号"，跟颜色不是一回事。没有多目标，就只能把几何**再画一遍**
//! 到另一张纹理去写法线（顶点/实例数据都得再来一次），或者干脆放弃逐像素样式改用
//! 预烘焙的渐变。有了 MRT，第二路数据是同一次绘制的副产物。
//!
//! ## 几个容易踩的点
//!
//! - 只有 **custom material** 能写多目标；**第 0 个字段必须叫 `color`**。
//! - 附加目标固定 `LoadOp::Load`，**不会自动清屏**：这里靠全写（`blend: None`）保证
//!   每帧每个像素都被覆盖一次。
//! - 法线目标也是 sRGB 8 位格式（和 surface 同格式），精度有限但够用；写入/采样
//!   的 sRGB 编解码互为逆运算，数值能原样往返。
//! - 文字不要放在挂了 custom material 的 batch 里，否则文字会走这个材质的 FS
//!   （这里把提示文字单独放一个 batch）。
//! - 光照需要在**同一坐标空间**里算：uv(0..1) 与像素混用会得到歪的光照方向，
//!   这里统一换算到像素空间。

use vireo::prelude::*;

const W: u32 = 760;
const H: u32 = 420;

/// Pass 1：一次写 albedo + 法线。
const GEOM_FRAG: &str = r#"
struct MaterialOutput {
    @location(0) color: vec4<f32>,
    @location(1) normal: vec4<f32>,
};

// 从 SDF 参数直接推法线：形状是解析的，不需要额外顶点数据。
//
// 关键点：法线来自**有符号距离的梯度**，且只在离边缘 `bevel` 宽的带子里倾斜，
// 形状内部保持完全纯平（n = +Z）。若按"归一化坐标线性铺满"算，平坦的正面会被
// 当成曲面（看起来像充气枕头），倒角半径也不参与、角上还会出现硬边。
fn shape_normal(in: MaterialInput) -> vec3<f32> {
    let p = in.local_pos - in.sdf_params.xy;
    let half = in.sdf_params.zw;
    let cr = in.sdf_extra.x;

    // 圆/椭圆：把归一化半径折算成像素距离（椭圆按短半轴折算）
    let rr = length(p / half) - 1.0;
    let sd_circle = rr * min(half.x, half.y);

    // 圆角矩形 SDF（像素，内部为负）
    let q = abs(p) - (half - vec2<f32>(cr));
    let sd_rect = length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - cr;

    // 两条分支都要算完再 select：dpdx/dpdy 必须处于一致控制流。
    let is_circle = in.sdf_type == 1u;
    let sd = select(sd_rect, sd_circle, is_circle);
    // 统一用**很窄**的倒角：正面纯平，只在离边几像素内起坡。上半球那种做法会把
    // 扁平 UI 变成塑料果冻（2000 年代审美）；窄边光才是现代观感。
    let bevel = min(half.x, half.y) * 0.9;   // 柔和的整体穹顶：给"斜向渐变"提供连续场

    // 梯度指向外侧（sd 增大方向）
    let g = vec2<f32>(dpdx(sd), dpdy(sd));
    let dir = g / max(length(g), 1e-6);

    let t = clamp(-sd / bevel, 0.0, 1.0);      // 0 = 边缘，1 = 内部
    let slope = 1.0 - smoothstep(0.0, 1.0, t);  // 边缘 1（完全侧向），内部 0（纯平）
    let nxy = dir * slope;
    return normalize(vec3<f32>(nxy, sqrt(max(1.0 - dot(nxy, nxy), 0.0)) + 0.001));
}

fn material_main(in: MaterialInput) -> MaterialOutput {
    let base = vireo_base_sample(in.uv);
    let n = shape_normal(in);
    var out: MaterialOutput;
    out.color = base * in.color;
    out.normal = vec4<f32>(n.xy * 0.5 + vec2<f32>(0.5), n.z * 0.5 + 0.5, 1.0);
    return out;
}
"#;

/// Pass 2：把法线当成**样式场**用，而不是光照模型。
///
/// 法线在这里做两件"平面设计"的事，而不是"打光"：
/// - **斜向柔光带**：法线投影到一个方向 → 形状内部一条低对比的渐变（mesh gradient）；
/// - **极窄内发光**：`(1-n.z)^8` 只让贴近边缘的一圈亮起来，跟着鼠标那一侧更亮。
///
/// 刻意不做 Lambert / Blinn-Phong / 环境压暗：那套"一盏灯照在模型上"的效果放在扁平
/// UI 上会立刻显得像上个世纪的皮肤。
const LIGHT_FRAG: &str = r#"
fn lum_at(uv: vec2<f32>) -> f32 {
    return dot(textureSample(scene, scene_samp, uv).rgb, vec3<f32>(0.299, 0.587, 0.114));
}

fn material_main(in: MaterialInput) -> vec4<f32> {
    let uv = in.uv;
    let albedo = textureSample(scene, scene_samp, uv).rgb;
    let n = normalize(textureSample(normal, normal_samp, uv).xyz * 2.0 - 1.0);

    let px_n = vec2<f32>(1.0 / 760.0, 1.0 / 420.0);
    let ppx = uv * vec2<f32>(760.0, 420.0);
    let to_l = light_px.xy - ppx;
    let to_ld = normalize(to_l + vec2<f32>(1e-4));

    // 背景近乎纯黑，用亮度当"是形状"的掩码
    let here = smoothstep(0.02, 0.06, dot(albedo, vec3<f32>(0.299, 0.587, 0.114)));

    // 1) 斜向柔光带：法线投影到固定方向，低对比（0.90~1.14），不出高光
    let sheen_dir = vec2<f32>(0.82, -0.57);
    let s = dot(n.xy, sheen_dir) * 0.5 + 0.5;
    let sheen = mix(vec3<f32>(1.15, 1.03, 0.93), vec3<f32>(0.88, 1.00, 1.16), s);
    var col = albedo * mix(vec3<f32>(1.0), sheen, here);

    // 2) 极窄内发光：只在 n.z 接近 0 的极边缘非零，所以是一圈细光，不是"面上打灯"
    let edge = pow(1.0 - clamp(n.z, 0.0, 1.0), 8.0);
    let facing = 0.5 + 0.5 * dot(normalize(n.xy + vec2<f32>(1e-5)), to_ld);
    col += vec3<f32>(0.55, 0.72, 1.00) * edge * facing * 0.85 * here;

    // 3) 接触阴影：沿"指向鼠标"的方向取样，命中形状说明本像素在它的影子里
    //    （反向取样会得到"物体上方有阴影"的反效果）
    var shade = 0.0;
    for (var i = 1; i <= 6; i++) {
        let o = to_ld * (f32(i) * 5.0) * px_n;
        shade += smoothstep(0.02, 0.06, lum_at(uv + o)) * (1.0 - f32(i) / 7.0);
    }
    col *= 1.0 - clamp(shade, 0.0, 1.0) * 0.45 * (1.0 - here);

    return vec4<f32>(col, 1.0);
}
"#;

#[vireo::main]
async fn main() {
    let window = app.window(
        WindowDesc::new("MRT: normal-driven sheen", W, H),
        None::<fn()>,
    );
    let scene = app.offscreen(W, H, AntiAliasing::None);
    let normals = app.offscreen(W, H, AntiAliasing::None);

    let geom = app.material(GEOM_FRAG).expect("geom material");
    let lit = app
        .material_with_resources(
            LIGHT_FRAG,
            MaterialResources(&[
                MaterialResource {
                    name: "scene",
                    kind: MaterialResourceKind::Texture {
                        view: TexKind::D2(TexSample::Float),
                        sampler: SampKind::Filtering,
                    },
                },
                MaterialResource {
                    name: "normal",
                    kind: MaterialResourceKind::Texture {
                        view: TexKind::D2(TexSample::Float),
                        sampler: SampKind::Filtering,
                    },
                },
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
        .expect("light material");

    let device = app.gpu().device.clone();
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("mrt_light sampler"),
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
        let normals = match ctx.app().offscreen_ref(&normals) {
            Ok(v) => v,
            Err(_) => return false,
        };
        t += 0.016;

        // ---- Pass 1：MRT 写 albedo + 法线 ----
        let mut geo = DrawBatch::new();
        geo.set_custom_material(Some(geom.clone()));
        draw_rectangle(
            &mut geo,
            Pos::new(0.0, 0.0),
            W as f32,
            H as f32,
            Some(Color::new(0.005, 0.006, 0.013, 1.0)),
        );
        draw_circle(&mut geo, Pos::new(140.0, 150.0), 56.0, Some(ORANGE));
        draw_rounded_rect(
            &mut geo,
            Pos::new(300.0, 90.0),
            180.0,
            120.0,
            28.0,
            Some(SKYBLUE),
        );
        draw_circle(&mut geo, Pos::new(620.0, 160.0), 52.0, Some(PINK));
        draw_rounded_rect(
            &mut geo,
            Pos::new(100.0, 280.0),
            190.0,
            110.0,
            24.0,
            Some(GREEN),
        );
        draw_rounded_rect(
            &mut geo,
            Pos::new(420.0, 270.0),
            230.0,
            120.0,
            30.0,
            Some(GOLD),
        );
        let normal_target = ColorTargetDesc {
            view: normals.texture.lock().view.clone(),
            resolve_target: None,
            blend: None,
        };
        scene.draw_mrt(Some(BLACK), &[normal_target], &[&geo]);

        // ---- 光源：鼠标位置；鼠标还没进来时绕圈 ----
        let (mx, my) = win.mouse_pos().logical();
        let (lx, ly): (f32, f32) = if mx < 0.0 || my < 0.0 {
            (
                W as f32 * 0.5 + (t * 1.2).cos() * 240.0,
                H as f32 * 0.5 + (t * 1.2).sin() * 130.0,
            )
        } else {
            (mx as f32, my as f32)
        };
        lit.set_uniform(&ctx.app().gpu().queue, "light_px", &[lx, ly, 0.0, 0.0]);

        // ---- Pass 2：光照合成 ----
        {
            let scene = scene.texture.lock();
            let normals = normals.texture.lock();
            lit.set_texture(&device, "scene", &scene.view, &sampler);
            lit.set_texture(&device, "normal", &normals.view, &sampler);
        }
        let mut blit = DrawBatch::new();
        blit.set_custom_material(Some(lit.clone()));
        draw_rectangle(
            &mut blit,
            Pos::new(0.0, 0.0),
            W as f32,
            H as f32,
            Some(WHITE),
        );

        // 提示文字单独一个 batch：挂在自定义材质的 batch 上会被那个 FS 着色。
        let mut hud = DrawBatch::new();
        draw_text(
            &mut hud.texts,
            "move the mouse",
            Pos::new(12.0, 10.0),
            TextDef::default().font_size(13.0),
            TextOverride::from_color(Color::new(0.75, 0.78, 0.85, 1.0)),
        );

        win.draw(BLACK, &[&blit, &hud]);
        true
    })
    .await
    .unwrap();
}
