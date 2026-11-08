//! 渐变展示示例：停点 / 角度 / 圆角 / 各形状 / 贴图与预乘。
//!
//! 四个区域，覆盖 `draw_*_gradient` 的全部能力：
//!
//! 1. **停点**：两色、平台 + 淡出、硬边（同 `at` 两个停点）、越界（`at` 超出 0..1，钳成平台）、
//!    多色。渐变坐标都是「沿轴归一化 0..1」。
//! 2. **角度**：0° = 自上而下，90° = 自左而右；渐变在**本地空间**计算，所以旋转过的矩形
//!    会带着渐变一起转。同一角度作用在矩形 / 圆角矩形 / 圆 / 椭圆 / 弧 / 多边形上。
//! 3. **线段 / 折线**：坐标由形状决定 —— 线段沿 `p1→p2`（没有角度参数），折线沿**弧长**
//!    （点距不均匀也均匀过渡）。
//! 4. **与贴图、预乘**：渐变 × 贴图 = 两者相乘；透明 ↔ 饱和色在预乘空间插值，白底上不出暗边；
//!    圆角裁剪与渐变正交；只有 1 个停点时退化成等价单色。
//!
//! 跑法：`cargo run --example gradient`（需显示器）。
//!
//! 逐点色的另一条用法（沿弧长逐点给色）见 `examples/line_gradient.rs`。
//!
//! 按键：
//! - `←→` 全部渐变的旋转角度
//! - `R`  圆角开关
//! - `T`  贴图开关（看第四区）
//! - `Q` / `ESC` 退出

use vireo::prelude::*;

const W: u32 = 900;
const H: u32 = 560;

fn gs(at: f32, c: Color) -> GradientStop {
    GradientStop::new(at, c)
}

fn clear() -> Color {
    Color::new(0.0, 0.0, 0.0, 0.0)
}

/// 面板标题（小字）。
fn title(b: &mut DrawBatch, text: &str, x: f32, y: f32) {
    draw_text(
        &mut b.texts,
        text,
        Pos::new(x, y),
        TextDef::default().font_size(12.5),
        TextOverride::from_color(Color::new(0.64, 0.69, 0.79, 1.0)),
    );
}

#[vireo::main]
async fn main(app: App) {
    let idx = app.window(WindowDesc::new("Vireo Gradients", W, H));

    let mut angle_deg = 0.0f32;
    let mut rounded = false;
    let mut textured = false;
    let mut checker: Option<Texture> = None;

    app.run(move |ctx| {
        let win = match ctx.app().window_ref(&idx) {
            Ok(v) => v,
            Err(_) => return true,
        };
        if win.key_down(KeyCode::Escape) || win.key_down(KeyCode::KeyQ) {
            return false;
        }
        if win.key_down(KeyCode::ArrowLeft) {
            angle_deg -= 1.5;
        }
        if win.key_down(KeyCode::ArrowRight) {
            angle_deg += 1.5;
        }
        if win.key_down(KeyCode::KeyR) {
            rounded = !rounded;
        }
        if win.key_down(KeyCode::KeyT) {
            textured = !textured;
        }
        if checker.is_none() {
            // 棋盘格贴图：看"渐变 × 贴图"
            let n = 32u32;
            let mut rgba = Vec::with_capacity((n * n * 4) as usize);
            for y in 0..n {
                for x in 0..n {
                    let on = ((x / 8) + (y / 8)) % 2 == 0;
                    let v = if on { 235u8 } else { 60u8 };
                    rgba.extend_from_slice(&[v, v, v, 255]);
                }
            }
            checker = Some(Texture::from_rgba(n, n, &rgba, ctx.app().gpu()));
        }

        let mut b = DrawBatch::new();
        let radius = if rounded { 18.0 } else { 0.0 };

        // ====== 1. 停点 ======
        title(&mut b, "1. 停点：at = 沿渐变轴归一化的位置（0..1，越界钳制）", 20.0, 14.0);

        let two = [
            gs(0.0, Color::new(1.0, 0.85, 0.30, 1.0)),
            gs(1.0, Color::new(0.86, 0.20, 0.36, 1.0)),
        ];
        b.rect_gradient(Pos::new(20.0, 34.0), 160.0, 58.0, LinearGradient::vertical(&two));
        title(&mut b, "(0,a) (1,b)", 20.0, 96.0);

        let plateau = [
            gs(0.0, Color::new(1.0, 1.0, 1.0, 1.0)),
            gs(0.55, Color::new(1.0, 1.0, 1.0, 1.0)),
            gs(1.0, clear()),
        ];
        b.rect_gradient(
            Pos::new(196.0, 34.0),
            160.0,
            58.0,
            LinearGradient::vertical(&plateau),
        );
        title(&mut b, "(0,a) (0.55,a) (1,b) 平台+淡出", 196.0, 96.0);

        let hard = [
            gs(0.0, Color::new(0.25, 0.55, 0.95, 1.0)),
            gs(0.5, Color::new(0.25, 0.55, 0.95, 1.0)),
            gs(0.5, Color::new(0.95, 0.55, 0.25, 1.0)),
            gs(1.0, Color::new(0.95, 0.55, 0.25, 1.0)),
        ];
        b.rect_gradient(Pos::new(372.0, 34.0), 160.0, 58.0, LinearGradient::vertical(&hard));
        title(&mut b, "两个同 at 的停点 = 硬边", 372.0, 96.0);

        let out_of_range = [
            gs(-0.5, Color::new(0.30, 0.85, 0.55, 1.0)),
            gs(1.5, Color::new(0.10, 0.20, 0.45, 1.0)),
        ];
        b.rect_gradient(
            Pos::new(548.0, 34.0),
            160.0,
            58.0,
            LinearGradient::vertical(&out_of_range),
        );
        title(&mut b, "at = -0.5 → 1.5", 548.0, 96.0);

        let four = [
            gs(0.0, Color::new(1.0, 0.30, 0.30, 1.0)),
            gs(0.33, Color::new(0.98, 0.85, 0.25, 1.0)),
            gs(0.66, Color::new(0.25, 0.85, 0.55, 1.0)),
            gs(1.0, Color::new(0.30, 0.45, 0.98, 1.0)),
        ];
        b.rect_gradient(Pos::new(724.0, 34.0), 156.0, 58.0, LinearGradient::vertical(&four));
        title(&mut b, "4 个停点", 724.0, 96.0);

        // ====== 2. 角度 + 形状 ======
        title(
            &mut b,
            &format!("2. 角度 {angle_deg:.0}°（0 = 自上而下，90 = 自左而右；本地空间，随形状旋转）"),
            20.0,
            126.0,
        );
        let ramp = [
            gs(0.0, Color::new(0.35, 0.75, 1.0, 1.0)),
            gs(1.0, Color::new(0.10, 0.14, 0.30, 1.0)),
        ];
        let g = LinearGradient::at(angle_deg, &ramp);
        b.rect_gradient(Pos::new(20.0, 148.0), 140.0, 76.0, g);
        b.rounded_rect_gradient(Pos::new(180.0, 148.0), 140.0, 76.0, 22.0, g);
        b.circle_gradient(Pos::new(372.0, 186.0), 38.0, g);
        b.shape_gradient(
            &Shape::Ellipse {
                pos: Pos::new(470.0, 186.0),
                rx: 48.0,
                ry: 30.0,
            },
            g,
        );
        // 旋转 30° 的矩形：渐变跟着转（本地空间语义）
        b.set_position(600.0, 186.0);
        b.set_deg(30.0);
        b.rect_gradient(Pos::new(-70.0, -38.0), 140.0, 76.0, g);
        b.set_deg(0.0);
        b.set_position(0.0, 0.0);

        b.shape_gradient(
            &Shape::Sector {
                pos: Pos::new(760.0, 186.0),
                r: 38.0,
                start: 0.5,
                end: 2.6,
            },
            g,
        );
        let hex = [
            (840.0, 160.0),
            (872.0, 160.0),
            (888.0, 186.0),
            (872.0, 212.0),
            (840.0, 212.0),
            (824.0, 186.0),
        ];
        b.shape_gradient(&Shape::Polygon { points: &hex }, g);

        // ====== 3. 线段 / 折线：坐标由形状决定 ======
        title(
            &mut b,
            "3. 线段沿 p1→p2（没有角度参数）；折线沿弧长（三段弧长 ≈120 / 66 / 180，颜色按弧长走）",
            20.0,
            254.0,
        );
        b.line_gradient(20.0, 288.0, 400.0, 288.0, 12.0, &two);
        let pts = [
            (450.0f32, 288.0f32),
            (570.0, 288.0),
            (630.0, 316.0),
            (810.0, 316.0),
        ];
        b.shape_gradient(
            &Shape::LineChain {
                points: &pts,
                thickness: 12.0,
            },
            LinearGradient::vertical(&two),
        );

        // ====== 4. 贴图 × 渐变 / 预乘 / 圆角 / 退化 ======
        title(
            &mut b,
            "4. 渐变 × 贴图（T 键）  ·  透明 ↔ 饱和色（预乘，白底无暗边）  ·  圆角（R 键）  ·  1 个停点 = 单色",
            20.0,
            352.0,
        );

        let fade = [
            gs(0.0, Color::new(1.0, 1.0, 1.0, 1.0)),
            gs(1.0, clear()),
        ];
        if textured {
            b.set_texture(checker.as_ref());
        }
        b.rect_gradient(Pos::new(20.0, 376.0), 200.0, 100.0, LinearGradient::vertical(&fade));
        b.set_texture(None);

        // 白底 + 饱和红 → 透明：直通空间插值会在低 alpha 处发灰，预乘不会
        draw_rectangle(
            &mut b,
            Pos::new(240.0, 376.0),
            200.0,
            100.0,
            Some(Color::new(0.92, 0.92, 0.94, 1.0)),
        );
        let red_fade = [
            gs(0.0, Color::new(0.98, 0.22, 0.28, 1.0)),
            gs(1.0, Color::new(0.98, 0.22, 0.28, 0.0)),
        ];
        b.rect_gradient(
            Pos::new(240.0, 376.0),
            200.0,
            100.0,
            LinearGradient::vertical(&red_fade),
        );

        let warm = [
            gs(0.0, Color::new(0.95, 0.85, 0.35, 1.0)),
            gs(1.0, Color::new(0.75, 0.20, 0.55, 1.0)),
        ];
        if rounded {
            b.rounded_rect_gradient(
                Pos::new(470.0, 376.0),
                200.0,
                100.0,
                radius,
                LinearGradient::vertical(&warm),
            );
        } else {
            b.rect_gradient(
                Pos::new(470.0, 376.0),
                200.0,
                100.0,
                LinearGradient::vertical(&warm),
            );
        }

        // 只有 1 个停点 ⇒ 与 draw_rectangle 逐像素一致的纯色
        let single = [gs(0.0, Color::new(0.40, 0.70, 0.90, 1.0))];
        b.rect_gradient(
            Pos::new(690.0, 376.0),
            190.0,
            100.0,
            LinearGradient::vertical(&single),
        );
        title(&mut b, "1 个停点 → 等价单色", 690.0, 482.0);

        // ====== HUD ======
        draw_text(
            &mut b.texts,
            "←→ 角度    R 圆角    T 贴图    Q 退出",
            Pos::new(20.0, 516.0),
            TextDef::default().font_size(13.0),
            TextOverride::from_color(Color::new(0.72, 0.77, 0.86, 1.0)),
        );

        win.draw(Color::new(0.06, 0.065, 0.085, 1.0), &[&b]);
        true
    })
    .await
    .unwrap();
}
