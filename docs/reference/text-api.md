# 文字：定义与绘制（API 参考）

> 本文负责**能定义什么、怎么画、怎么量**。缓存行为（自动/自行管理、预算、持久化）见
> [text-cache.md](text-cache.md)；字体见 [text-fonts.md](text-fonts.md)；样式见
> [text-style.md](text-style.md)；性能见 [text-perf.md](text-perf.md)。
>
> 概念与术语：[text.md](text.md)。

## 1. `TextDef`：文字长什么样

```rust
pub struct TextDef {
    pub font_size: f32,                    // 逻辑像素，默认 16.0
    pub max_width: Option<f32>,            // 断行宽度（逻辑像素）；None = 不折行
    pub align: TextAlign,                  // 只有 max_width 生效时才有意义
    pub attrs: Option<AttrsOwned>,         // 字体族 / 字重 / 斜体；None = 默认
    pub style: Option<Arc<TextStyle>>,     // 描边/投影/渐变…；None = 零额外开销
}
```

构造（链式）：

| 方法 | 说明 |
|---|---|
| `TextDef::default()` | 16px、不折行、左对齐、无样式 |
| `.font_size(f32)` | 逻辑像素；配合窗口 DPI 换算成物理像素 |
| `.max_width(f32)` | 超过就折行；折行后 `align` 才生效 |
| `.align(TextAlign)` | `Left` / `Center` / `Right` / `Justified` / `End`（见 §7） |
| `.with_family(Family)` | `Family::Name("...")` / `Family::Monospace` 等，透传给 cosmic-text |
| `.with_weight(Weight)` | `Weight::NORMAL` / `BOLD` / 数值 |
| `.with_style(Style)` | `Style::Normal` / `Italic` / `Oblique` |

字段是公开的，也可以直接构造：

```rust
let def = TextDef {
    style: Some(Arc::new(TextStyle::outline(3.0, ink))),
    ..TextDef::default().font_size(28.0)
};
```

**行距固定**为 `font_size × 1.2`（`Metrics::new(font_size, font_size * 1.2)`），目前没有可配入口。

`TextDef` 参与**排版缓存的键**：改字号 / 折行宽度 / 对齐 / attrs / style 都会让同一段文字
重新 shape。位置（`Pos`）不在键里。

## 2. `TextOverride`：这一次绘制怎么画

```rust
pub struct TextOverride {
    pub color: Option<Color>,
    pub clip: Option<Option<TextBounds>>,          // 三层语义，见下
    pub transform: Option<Transform>,              // 叠加在 batch 变换之上
    pub uv: Option<UvRect>,                        // 覆盖 batch 的 base uv
    pub bind_group: Option<Option<wgpu::BindGroup>>, // Some(None) = 白贴图
}
```

| 字段 | `None` | `Some(..)` |
|---|---|---|
| `color` | 用 `DrawBatch::color` | 覆盖这一次的文字颜色 |
| `clip` | 用 `DrawBatch::text_clip` | `Some(Some(b))` 用矩形 `b`；`Some(None)` **清除裁剪**（整条都画） |
| `transform` | 用 batch 当前变换 | 在 batch **局部空间右乘**一个变换，不污染 batch 状态 |
| `uv` | 用 batch 的 uv | 覆盖文字采样的 base uv（纹理区域） |
| `bind_group` | 用 batch 的纹理 | `Some(Some(bg))` 指定 base 纹理；`Some(None)` 白贴图 |

构造：`TextOverride::new()` / `TextOverride::from_color(c)` 与 `.color(..)` `.clip(l,t,r,b)`
`.clear_clip()` `.transform(t)` `.uv(..)` `.uv_rect(..)` `.texture(&tex)` `.clear_texture()`
`.bind_group(..)`。

`clip` 是**逻辑像素**，`prepare` 时 × `layout_scale()` 成物理；默认值
`TextBounds { i32::MIN..i32::MAX }` 表示不裁剪。裁剪是**逐字形 CPU 剔除**（被裁掉的字形既
不进顶点也不进图集），与 `scissor`/`Area` 的 GPU 裁剪是两件事。

## 3. 绘制入口

| 写法 | 吃批次变换？ | 用在哪 |
|---|---|---|
| `draw_text(&mut batch.texts, ..)` | **否**（固定 `transform_index = 0`） | 屏幕空间 HUD、屏幕坐标 |
| `batch.text(..)` | 是 | 跟随 `set_position` / `set_rad` / `set_scale` |
| `batch.text_parts(..)` | 是 | 同上，但内容分段（HUD 数字） |
| `batch.text_stable(&stable, ..)` | 是 | 大段**静态**文字（实例固化，见 §6） |
| `batch.text_hud(..)` / `batch.hud_line(..)` | 是 | HUD 行的糖（见 §4） |

底层还可以直接操作 `TextEntryList`：`push` / `push_parts` / `push_stable` / `push_hud`，
以及 `push_*_indexed` 版本（显式指定 `transform_index`）。`draw_text` / `draw_text_parts` /
`draw_text_hud` 就是它们的便捷包装。

**变换的语义**：`batch.text(..)` 记录入队时的批次变换下标，渲染时先乘批次物理变换再进屏幕
空间。`draw_text` 刻意固定下标 0（恒等），所以放在带变换的批次里也不会跟着动 —— 这是历史
行为，需要跟随就用 `batch.text(..)`。

## 4. 分段文字与 HUD 行

内容是"标签 + 会变的数字"时，整串 `format!` 每帧都会产生新的排版键。分段后标签段可以命中
缓存，数字段只走窄路径：

```rust
// 手写分段
draw_text_parts(
    &mut batch.texts,
    &[TextPart::normal("FPS: "), TextPart::glyphs(fps.to_string())],
    Pos::new(16.0, 16.0),
    TextDef::default().font_size(18.0),
    TextOverride::from_color(WHITE),
);

// 或让库按"数字/非数字"启发式切（split_hud 返回 Vec<TextPart>）
let parts = split_hud(&format!("FPS: {fps}  {ms:.2} ms"));
batch.text_parts(&parts, pos, def, ov);

// 或者用 builder：预留槽位、每帧只改数字（避免每帧重建 Vec<String>）
let mut line = HudLine::new().text("FPS: ").glyphs("--");
line.write_slot(1, &fps.to_string());
batch.hud_line(&line, pos, def, ov);
```

`HudLine` 的接口：`new()` / `text(s)` / `dynamic(s)` / `glyphs(s)` / `set_text(i, s)` /
`set_dynamic(i, s)` / `set_glyphs(i, s)` / `write_slot(i, s)` / `parts()` / `draw(list, ..)`
（`batch.hud_line` 就是 `draw` 的包装）。

`TextPart` 四种：

| 变体 | 含义 | 缓存 | 适合 |
|---|---|---|---|
| `Normal(String, Option<TextDef>)` | 内容稳定 | 整段 shape 命中 | 标签、说明 |
| `Dynamic(String, Option<TextDef>)` | 内容会变但仍按整段 shape | 字符串一变就 miss | 需要整段 shaping 的短句 |
| `Glyphs(String, Option<TextDef>)` | 任意字符按字解析，用**字体自身 advance** | 按字符缓存解析结果 | 数字、会变的短串 |
| `Stable(StableText)` | 预 shape 的句柄 | 排版 + 已解析字形 + 已烘焙实例 | 大段静态文字 |

分段约定：

- 段与段之间**不做 kerning/连字**（每段独立 shape），所以"数字紧贴字母"的场景可能比整串
  略紧/略松；视觉要求严格时用 `Normal` 整串或 `TextPart::normal_def` 调字距。
- 每段可以用自己的 `TextDef`（`normal_def`/`dynamic_def`/`glyphs_def`），行级 `TextDef` 作为默认。
- 已有 `StableText` 时用 `TextPart::stable(&h)` 把整块塞进一行。
- `HudLine` 也可以 `.parts()` 取出切片交给 `batch.text_parts(..)`，或直接
  `batch.hud_line(&line, pos, def, ov)` 一次画完（位置由行内累加）。

## 5. 度量

```rust
let (w, h) = gpu.text().measure("Hello 世界", &def);        // 排版尺寸（逻辑像素）
let (vw, vh) = gpu.text().measure_visual("Hello 世界", &def); // 视觉外框（含样式外扩）
```

| 方法 | 含义 |
|---|---|
| `gpu.text().measure(text, &def)` | 排版尺寸（逻辑像素，不含样式外扩） |
| `gpu.text().measure_visual(text, &def)` | 视觉外框（加上描边宽度 + 3σ 软边 + 偏移） |

排版尺寸对 `max_width` 敏感（按折行算多行高度）；`measure_visual` 里的 `Px` 意图按 1:1 处理
（没有 DPI 上下文），需要设备像素时自己乘 `layout_scale()`。两者都会把文字 shape 进缓存，
所以"先量再画"不会重复付排版成本。

## 6. 稳定句柄：`StableText`

```rust
let h = gpu.text().stable("很久不变的一大段文字", &TextDef::default().font_size(20.0));
h.bake(&gpu, win.layout_scale());            // 可选：加载阶段先烘焙实例
batch.text_stable(&h, Pos::new(24.0, 64.0), TextOverride::default());
```

（`stable` 是创建句柄的唯一入口。）

**它缓存什么**：排版结果（`Buffer`）、解析后的字形列表、以及**烘焙好的渲染实例**。实例是
**本地坐标**，每帧只做"拷实例 + 打补丁（整数位移 / 变换槽 / 颜色 / batch UV / 样式表下标）"。

失效与代价：

| 变化 | 会重烘吗 |
|---|---|
| 位置（含小数）/ 滚动 / 拖动 | **不会**（位置走变换槽） |
| 改颜色 | 不会 |
| DPI 变化 | 会 |
| `text_clip`（批次或本条）变化 | 会 |
| 带变换 ↔ 不带变换的批次 | 会 |
| 图集扩/缩/淘汰/清空/装载 | 会 |

唯一的质量取舍：**抗锯齿相位固定在烘焙时**。位置落在物理像素网格上时与普通绘制逐像素
相同；带小数时绘制位置相差 ≤1 物理像素（滚动反而更平滑）。需要相位逐帧精确就用
`batch.text(..)`。

观测与手动失效：`h.bakes()`（累计烘焙次数）、`h.baked_instances()`、`h.invalidate_bake()`、
`h.text()` / `h.font_size()` / `h.line_width()` / `h.line_count()`。

实测：12 000 字的块，逐字形 prepare 约 2.0 ms/帧，实例复用约 0.24 ms/帧
（见 [text-perf.md](text-perf.md)）。

## 7. 对齐与折行

- `max_width` 是折行宽度（逻辑像素）；不设就单行。
- `align` 只有折行后才有效果：`Left` / `Center` / `Right` / `Justified` / `End`。
- 折行由 cosmic-text 的简单换行完成，不做断词/连字符；超长英文单词会溢出。
- `StableText` 在创建时定型，`max_width=None` 时强制左对齐（避免无意义的居中）。

## 8. 与其它子系统的交互

- **文字自定义材质**：普通材质（`create_material(..)` / `app.material(..)`）加在批次上后会同时
  作用于形状与文字；shader 里用 `in.target_type == VIREO_TARGET_TEXT` 区分，文字可读
  `in.color` / `in.base_uv`。示例：`examples/custom_material_text.rs`；契约见
  [material.md](material.md)。
- **MRT**：文字管线按 pass 的颜色附件数取变体，附加附件用空写掩码保留内容；`PreparedFrame`
  也能绑多附件。
- **Stencil / Area**：文字可以画进带 stencil 的 pass，但 `Area` 的 SDF 掩码是给形状路径算的；
  要在 CPU 侧裁掉字，用 `TextOverride::clip` 或批次 `text_clip`（GPU 侧裁剪用 scissor）。
- **深度**：文字实例 `depth = 0`，层叠顺序由入队顺序决定。
- **颜色空间**：文字采样按附件格式决定是否做 sRGB 转换；`TextOverride::color` 直接写进实例。

## 9. 已知限制

- **行距不可配**：固定 `font_size × 1.2`。
- **分段之间不 kerning**：见 §4。
- **`TextPart::Glyphs` 用字体自身 advance**：等宽字体才等宽；数字比例字体下宽度会变。
- **换行不做断词**：超长单词溢出。
- **样式带宽上限**：外扩超过约 1.45em 会被截断并 warn 一次，见 [text-style.md](text-style.md)。
- **样式是逐段定型的**：`TextPart` 的段级 `TextDef` 决定该段的样式，同一行不同段可以有
  不同样式；但一段内部不能逐字换样式（要逐字就拆段）。

## 10. 方法速查

### `GpuContext`（文字相关）

只有一个入口：

| 方法 | 作用 |
|---|---|
| `text() -> &TextSystem` | 文字系统的统一入口 —— 字体、度量、缓存、句柄、预热、图集统计全部从这走 |

> `GpuContext` 上**不保留**任何文字方法的转发（0.2.0 起）。写 `gpu.text().<方法>` 即可。

### `TextSystem`（`gpu.text()`）

| 方法 | 作用 |
|---|---|
| `measure(text, &def)` / `measure_visual(text, &def)` | 排版尺寸 / 视觉外框（含样式外扩） |
| `stable(text, &def)` | 创建 `StableText` 句柄（预 shape，跨帧复用） |
| `preheat(&device, &queue, w, h)` | 首帧懒初始化（需 renderer 已 resize） |
| `load_font(bytes)` / `load_font_file(path)` | 加载自定义字体（会清排版与解析缓存） |
| `glyphs() -> GlyphCache<'_>` | 字形缓存视图（见下） |
| `atlas_stats()` | 图集尺寸/字节/条目数（`TextAtlasStats`） |
| `set_typeset_ttl` / `typeset_ttl` / `set_typeset_max_entries` / `typeset_max_entries` | 排版缓存旋钮 |
| `clear_typeset_cache` / `typeset_len` / `typeset_held_count` / `typeset_stats` / `reset_typeset_stats` | 排版缓存观测与清空 |
| `clear_resolved_glyph_cache` / `resolved_glyph_cache_len` | 逐字解析缓存（`TextPart::Glyphs` 用） |

### `GlyphCache`（`gpu.text().glyphs()`）

| 方法 | 作用 |
|---|---|
| `stats()` | O(1) 聚合（字段见 [text-cache.md §2.1](text-cache.md#21-观测)） |
| `entries()` | O(n) 逐条快照（图集坐标、种类、字节、钉住、in_use、键） |
| `pinned_count()` | 钉住的条目数 |
| `set_retention_budget` / `set_hard_budget` / `retention_budget` / `hard_budget` / `effective_budget_bytes` | 预算（语义见 [text-cache.md §2.3](text-cache.md#23-预算保留-vs-硬)） |
| `set_auto_compact` / `auto_compact` | 自动压缩开关 |
| `prewarm(&device, &queue, text, &def, scale)` | 只填图集，返回新光栅化条目数 |
| `hold(&device, &queue, text, &def, scale) -> GlyphHold` | 填图集 + 钉住（引用计数） |
| `trim()` / `clear_unpinned()` / `clear()` | 按预算淘汰 / 清未钉住 / 全清 |
| `compact(&device, &queue)` | 手动缩图集（放两帧之间） |
| `save(&device, &queue, path)` / `load(&device, &queue, path)` | `VRTX` 跨进程持久化 |

`stats()` 返回值上的便捷方法：`used_bytes()` / `texture_bytes()` / `entries() -> u32`；
`GlyphHold` 上有 `len()` / `is_empty()` / `release()`（`drop` 即解钉）。

### `StableText`

| 方法 | 作用 |
|---|---|
| `bake(&gpu, scale)` | 预烘焙实例（返回实例数；加载阶段用） |
| `bakes()` | 累计烘焙次数（稳态不该涨） |
| `baked_instances()` | 当前块内实例数（`None` = 未烘） |
| `invalidate_bake()` | 强制下次绘制重烘 |
| `text()` / `font_size()` / `line_width()` / `line_count()` | 只读元信息 |

### 分段与入队

| 方法 | 作用 |
|---|---|
| `TextPart::normal` / `dynamic` / `glyphs` / `stable`（各有 `*_def` 变体） | 构造段 |
| `split_hud(&str) -> Vec<TextPart>` | 按数字/非数字启发式切段 |
| `HudLine::new()` + `.text` / `.dynamic` / `.glyphs` / `.set_text` / `.set_dynamic` / `.set_glyphs` / `.write_slot` / `.parts` / `.draw` | HUD 行 builder |
| `TextEntryList::new` / `clear` / `new_from_entries` / `push` / `push_parts` / `push_stable` / `push_hud` / `draw` | 底层入队（`*_indexed` 版本可显式指定 `transform_index`） |
| `DrawBatch::text` / `text_parts` / `text_stable` / `text_hud` / `hud_line` | 跟随批次变换的绘制 |
| 自由函数 `draw_text` / `draw_text_parts` / `draw_text_hud` | 不吃批次变换的绘制 |
