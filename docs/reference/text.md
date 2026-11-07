# 文字（Text）

> 面向 vireo 使用者。适用版本：0.2.0。
>
> 相关：[文本样式](text-style.md)（描边 / 投影 / 渐变字）、[渐变](gradient.md)。

## 1. 三层模型

文字在 vireo 里分三层，**各管一件事，互不替代**：

| 层 | 管什么 | 入口 | 缓存 |
|---|---|---|---|
| **排版**（typeset / 排版） | 字体挑选、断行、字形下标与步进 | `draw_text` / `TextEntryList` / `StableText` | `typeset_ttl` / `typeset_max_entries` |
| **样式**（style） | 描边、投影、发光、内阴影、渐变、几何描边 | `TextStyle` | 按 `(字形, 尺寸, 带宽档)` 复用图集条目 |
| **光栅化**（glyph cache / 字形缓存） | 字形位图 / 距离场进图集 | `gpu.text().glyphs()` | 预算 + `hold` 钉子 + 跨进程 `save`/`load` |

一句话区分两个缓存：

- **排版缓存**管"这段字排一次"（`harfrust` 排版成本，典型 5–30ms/长串）；
- **字形缓存**管"这个字形画一次"（`swash`/SDF 光栅化成本，典型 0.05–0.5ms/字形）。

## 2. 术语表

vireo 的文档/代码里这三个词是**互不混用**的，遇到时按下表理解：

| 词 | 中文 | 指什么 | 反例（别混） |
|---|---|---|---|
| `typeset` | 排版 | 文字的布局结果：Shape 阶段产物、`Buffer`、行宽、字形序列 | 不是 UI 布局 |
| `layout` | 布局 | UI 的摆放（窗口、面板、对齐），本库只有 `layout_scale` 这类概念 | 不是文字排版 |
| `shape` | 图形/几何 | 矩形、圆、贝塞尔、SDF 图元 | 不是文字 |

历史包袱：文字相关的 API 早期叫 `shape_cache_*`（"shape 文本"），0.2.0 起改名为
`typeset_*`；旧名保留一个版本作为兼容壳（`set_shape_cache_ttl` 等），下一版删除。

## 3. 画一段字

```rust
use vireo::prelude::*;

// 1) 最简单：塞进批次
let mut batch = DrawBatch::new();
draw_text(
    &mut batch.texts,
    "Hello 世界",
    Pos::new(24.0, 32.0),
    TextDef::default().font_size(28.0),
    TextOverride::from_color(WHITE),
);
win.draw(Color::new(0.06, 0.06, 0.08, 1.0), &[&batch]);
```

两种入口的差别很关键：

| 写法 | 是否吃批次变换 | 用在哪 |
|---|---|---|
| `draw_text(&mut batch.texts, ..)` | **否**（固定 `transform_index = 0`） | 屏幕空间 HUD、固定位置 |
| `batch.text(..)` / `batch.text_stable(..)` / `batch.text_parts(..)` | **是**（跟随 `set_position`/`set_rad`/`set_scale`） | 跟随父节点变换的文字 |

`TextDef` 是可克隆的轻量配置：`font_size`、`max_width`（断行宽度）、`align`、
`attrs`（字体族/字重/字宽）、`style`（见 [text-style.md](text-style.md)）。
`TextOverride` 管这次绘制的覆盖项（颜色、裁切等）。

### 分段文字（HUD 数字专用）

```rust
draw_text_parts(
    &mut batch.texts,
    &[TextPart::normal("FPS: "), TextPart::glyphs(fps.to_string())],
    Pos::new(16.0, 16.0),
    TextDef::default().font_size(18.0),
    TextOverride::from_color(WHITE),
);
```

`TextPart::glyphs` 走**单字符窄路径**（等宽步进 + 按字符解析字形），不产生整段排版键，
所以"每帧都在变的数字"不会把排版缓存撑爆。宽度不齐的场合用 `TextPart::normal`。

### 稳定句柄

```rust
// 预排版一次，之后每帧复用（StableText 活着，Buffer 就不会被回收）
let title = gpu.make_stable_text("标题", &TextDef::default().font_size(32.0));
batch.text_stable(&title, Pos::new(24.0, 24.0), TextOverride::default());
```

别名：`gpu.text().stable(..)`。

## 4. 度量

| 方法 | 含义 |
|---|---|
| `gpu.measure_text(text, &def)` / `gpu.text().measure(..)` | 排版尺寸（逻辑像素，不含样式外扩） |
| `gpu.measure_text_visual(text, &def)` / `gpu.text().measure_visual(..)` | 视觉外框（加上描边宽度 + 3σ 软边 + 偏移） |

排版尺寸对 `max_width` 敏感（会按断行算多行高度）；`measure_text_visual` 里 `Px` 意图
按 1:1 处理（没有 dpi 上下文），需要设备像素时自己乘 `win.layout_scale()`。

## 5. 排版缓存（typeset）

```rust
gpu.text().set_typeset_ttl(Some(Duration::from_secs(2))); // None = 永不按时间回收
gpu.text().set_typeset_max_entries(Some(4096));           // None = 不限条数
let stats = gpu.text().typeset_stats();                   // hits/misses/gc_runs/last_gc_us
gpu.text().clear_typeset_cache();                          // 立即清空（StableText 不受影响）
```

- GC 用**真实时间**（与帧率无关），两次 GC 最短间隔 250ms；
- 被 `StableText` 持有的槽不会因 TTL/LRU/`clear_typeset_cache` 被回收
  （`typeset_held_count()` 可查）；
- GC 只在 `typeset_ttl` 有值时跑。默认硬顶 4096 条、软目标 512 条、TTL 2s。

旧名对照：`set_shape_cache_ttl` → `set_typeset_ttl`、`clear_shape_cache` →
`clear_typeset_cache`、`shape_cache_stats` → `typeset_stats`，其余同理。

## 6. 字形缓存（glyph cache）

入口是 `gpu.text().glyphs()`（返回 `GlyphCache<'_>`，借用 `GpuContext`，不能跨线程保存）。

### 6.1 观测

```rust
let g = gpu.text().glyphs();
let s = g.stats();
println!(
    "条目 {} / 占用 {} KiB / 纹理 {} KiB / 钉住 {} / 光栅化 {} / 淘汰 {} / 扩图 {}",
    s.entries(), s.used_bytes() / 1024, s.texture_bytes() / 1024,
    s.pinned_entries, s.rasterizations, s.evictions, s.grows,
);
for e in g.entries() {                 // 逐条快照，O(n)，别每帧调
    println!("{:?}/{:?} {}×{} @({},{}) {}B pinned={} in_use={} {}",
        e.atlas, e.content, e.width, e.height, e.x, e.y, e.bytes, e.pinned, e.in_use, e.key);
}
```

`GlyphCacheStats` 字段：`mask_entries` / `color_entries` / `pinned_entries` /
`mask_used_bytes` / `color_used_bytes` / `mask_texture_bytes` / `color_texture_bytes` /
`budget_bytes` / `rasterizations` / `evictions` / `grows` / `trims` / `budget_warnings`，
另有便捷方法 `entries()` / `used_bytes()` / `texture_bytes()`。

约定：`*_used_bytes` 是**条目面积累计**（不含图集里的空洞）；`*_texture_bytes` 是
**纹理容量**（`边长² × 通道数`，图集按 2 的幂翻倍，所以通常比占用大一圈）。

### 6.2 预算

```rust
g.set_budget(Some(8 * 1024 * 1024)); // 两张图合计 8 MiB
g.set_budget(None);                  // 不限制（默认）
```

帧首检查（`advance_frame` 内）：超预算就淘汰**未钉住**的 LRU 字形，直到回到预算内。
两条它不会做的事：

- **不会**为满足预算而丢弃 `hold` 住的字形 —— 全是钉子时只 `log::warn!` 一次
  （`stats().budget_warnings == 1`），图集继续用它需要的空间；
- **不会**缩小纹理本身（那是 [`compact`](#64-收缩) 的活）。

### 6.3 预光栅化与钉子

```rust
let g = gpu.text().glyphs();
let scale = win.layout_scale() as f32;

// 只填图集，不产生任何可见输出
let new = g.prewarm(&gpu.device, &gpu.queue, "加载中…", &def, scale);

// 填图集 + 钉住：句柄活着期间这批字形不会被 LRU/预算淘汰
let hold = g.hold(&gpu.device, &gpu.queue, "标题", &def, scale);
// ... 用很久 ...
drop(hold); // 解钉
```

- `prewarm` 返回"本次新光栅化的条目数"（0 = 全命中）；
- `hold` 内部先 `prewarm`，所以**钉住的键一定已经在图集里**（否则钉住没意义）；
- 同一批字形被多个句柄钉住是**引用计数**语义，最后一个句柄释放才解钉；
- 钉住的是**光栅化条目**，不是排版结果 —— 想要"排版也别重排"就配 `StableText`。

**键的粒度提醒**：图集条目按 `(字形, 生成尺寸/字号, 子像素档, 样式带宽档)` 存。
所以预光栅化能命中的前提是**位置的小数部分落在同一子像素档**——整数坐标画的字
天然落在 `Zero` 档，和 `prewarm` 的坐标一致；把文字放在 `x = 12.37` 这类位置会产生
另外的档位（最多 4×4 种/字号），需要就多画几个位置或干脆先画一帧。

### 6.4 收缩

```rust
g.clear_unpinned();                        // 清掉未钉住的条目，返回条数
g.clear();                                 // 全清（含钉住的）
let shrunk = g.compact(&gpu.device, &gpu.queue); // 图集收到刚好装下现有条目
```

清掉一批字形后图集尺寸**不会自己缩回去**；`compact` 把条目拷贝重排到更小的纹理
（不重新光栅化），把显存真正还回去。它会改变条目在图集里的坐标，所以**只在两帧之间调**。

### 6.5 跨进程持久化

```rust
// 退出前（或加载界面结束时）
let n = g.save(&gpu.device, &gpu.queue, "cache/glyphs.vrtx")?;

// 下次启动、刚建好上下文时
g.load(&gpu.device, &gpu.queue, "cache/glyphs.vrtx")?;
```

- 格式 `VRTX` v1，内容 = 字体表（postscript 名 + 字体数据摘要）+ 每条字形的
  `(键, 尺寸, bearing, 像素)`；
- **字体靠摘要认**：`fontdb::ID` 是进程内的 slotmap 键，跨启动必然不同，所以不能存 ID。
  装载时按名字找候选面、再验摘要，认不出的条目**跳过**（`Ok(条数)`，不是错误）；
  摘要对字体数据做**采样**（长度 + 头尾各 64KiB + 中间等距 64KiB）—— 整份哈希在中文
  字体上要 20ms/个，采样后约 0.2ms；
- **装载要求图集为空**（`glyphs().clear()` 之后，或刚启动还没画字），否则返回
  `ErrorKind::InvalidInput`；
- 装载会按面积重新打包（`etagere` 无法把块固定在指定坐标），顺带得到无碎片图集；
- 文件体积 ≈ 所有条目像素之和（R8 条目 `w×h`，彩色条目 `w×h×4`）；没有压缩；
- 两端都**批量提交**：保存只读回"用到的矩形"（2 的幂图集里通常只有几十 KB），
  装载走一个 staging buffer + 一次 `submit`（逐条 `write_texture` 会反复等暂存带）。
  一次设备等待（`poll(Wait)`）不可避免，它会把队列里已提交的绘制也排空；
- 版本/魔数不符、文件截断 → `ErrorKind::InvalidData`，图集保持原样。

**实测**（Win11 / DX12 / release，105 条 32px 字形；跑
`cargo test --release --test gpu_bench -- --ignored --test-threads=1 glyph_cache_perf --nocapture`）：

| 操作 | 耗时 | 备注 |
|---|---|---|
| `prewarm` 冷 | 0.03–0.05 ms/字 | 32px 拉丁+中文混排 |
| `prewarm` 命中 | ~1 µs/字 | 48 条全命中 0.03–0.05 ms |
| `save` | 29 ms（0.28 ms/条，75 KiB） | 含一次设备排空（参考 11 ms） |
| `load` | 13–14 ms（0.13 ms/条） | 读盘 + 上传 |
| 首帧（装载后 vs 空图集） | 30 ms vs 38–54 ms | 省 20–40%，空图集要现场光栅化 48 条 |
| `compact` | ~0.01 ms（无需收缩时） | 有条目要搬时与条目数成正比 |

## 7. 性能建议

1. **加载阶段预光栅化**：`prewarm` 一遍加载界面/主菜单会出现的文案，之后每帧纯命中。
   放在 `app.run` 之前或首个 `prewarm` 调用里都可以（视口没建时会自动用占位分辨率）。
2. **钉住骨架字形**：标题、固定标签用 `hold`，它们既不重光栅化也不挤占预算。
3. **给个预算**：UI 文案会变的场景（数字、玩家名）设个 4–16 MiB 预算，
   图集就不会按历史峰值常驻。
4. **想要跨帧零重光栅化**：用固定字号 + 整数坐标 + `StableText`/`prewarm`；
   跑 `--example text_cache` 按空格暂停，`rasterizations` 不动就说明做到了。
5. **别每帧调 `entries()`**：它是 O(条目数) 的全量快照，`stats()` 才是 O(1) 聚合。
6. **量一遍再调**：`cargo run --example text_profile` 看排版/光栅化分项；
   `cargo test --release --test gpu_bench -- --ignored --test-threads=1 glyph_cache_perf --nocapture`
   打印本机上的 prewarm / grow / save / load / compact 数字（§6.5 那张表就是它的输出）。

## 8. 已知限制

- **带宽上限**：样式的外扩（描边 + 软边 + 偏移）超过带宽上限（44px 参考字号下约 64px，
  即约 1.45em）时会被切短，并在首次出现时 `log::warn!` 一次。这是刻意设的上限
  （见 [text-style.md](text-style.md) §7），不是 bug。
- **彩色字形（COLRv1 / emoji）**：进独立的 RGBA8 图集；样式只作用于它的"形状"
  （从 alpha 反推的距离场），不改它自己的颜色。
- **`hold` 不解钉就丢不掉**：句柄 `drop` 即解钉；忘了 drop 的字形会一直占预算，
  `stats().pinned_entries` 是唯一的提示。
- **持久化不含排版缓存**：`VRTX` 只有光栅化结果。跨启动省掉排版成本需要自己保存文案
  并用 `StableText` 重建。
- **持久化对字体的要求**：换了字体文件（哪怕同名不同版本）摘要就不匹配 → 该字体的
  条目全部跳过并重新光栅化（正确但省不到时间）。

## 9. 相关示例

| 示例 | 演示 |
|---|---|
| `cargo run --example text_typeset_cache` | 排版缓存：TTL / 条数上限 / 命中率 / 动态串压力 |
| `cargo run --example text_cache` | 字形缓存：预算 / `hold` / `trim` / `compact` / `save` / `load` |
| `cargo run --example text_style` | 样式：描边、投影、发光、多层、渐变、几何描边 |
| `cargo run --example text_hud` | HUD 分段 + 数字窄路径 |
| `cargo run --example text_profile` | 排版/光栅化的分项耗时 |
| `cargo run --example text_measure` | `measure_text` / `measure_text_visual` |
