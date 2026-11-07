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

> **预算必须 ≥ 单帧工作集的占用**，否则会每帧重光栅化。同一个场景（60 段互不重复的 12 字
> 文本、24px、每帧全画）在三处实测：
>
> | 环境 | 工作集 | 预算 < 工作集 | 预算够 | 倍数 |
> |---|---|---|---|---|
> | 桌面无头（Win11/DX12, release） | 371 KiB | 8.7–12.3 ms/帧、**479 次/帧**光栅化 | 0.4–0.9 ms/帧、0 次 | ~10–20× |
> | Windows 真窗口（DPI 2、120 Hz vsync） | 1518 KiB | **18.5–20.0 ms/帧（50–54 fps）**、768 次/帧 | 8.27 ms（120.8 fps）、0 次 | **2.2–2.4×** |
> | Android 模拟器（dpi 1.5、60 Hz） | 828 KiB | **134.6–137.7 ms/帧（7.3–7.4 fps）**、635 次/帧 | 16.45–16.50 ms（60.7 fps）、0 次 | **8.1–8.4×** |
>
> 真窗口的倍数看起来小，是因为基线被 vsync 钉在 8.3 ms（120 fps）——它掉到了 56 fps，
> 也就是"已经开始掉帧"了。工作集随 `scale²` 长：桌面 200% DPI 下同样文案是 1.5 MiB，
> 不是 371 KiB。
>
> 结论：拿 `stats().peak_used_bytes`（**单帧占用峰值**，帧首读数取 max）当工作集，
> 预算留 1.5–2× 余量。预算小于它时 `set_budget` 会 `log::warn!` 一次。
>
> **"掉到 50 fps"到底是什么**（Windows 真窗口、DPI 2、120 Hz，用
> `DrawReport::timings` 拆成 `编码` 与 `等待`）：
>
> | 预算 | 帧耗时 | = 编码（记录+prepare+提交） | + 等 vsync | 光栅化 |
> |---|---|---|---|---|
> | 256 KiB（< 峰值 1518 KiB） | 20.20 ms（49.5 fps） | **20.06 ms** | 0.11 ms | **768 次/帧** |
> | 8 MiB | 8.29 ms（120.6 fps） | 0.74 ms | 7.53 ms | 0 |
>
> 同一场景关掉垂直同步（`VIREO_TEXT_CACHE_NOVSYNC=1`）：抖动态 18.01 ms（编码 17.90）、
> 稳态 0.96 ms（编码 0.79，≈1038 fps）。三点结论：
>
> 1. **不是 GPU、也不是 vsync 量化**：50 fps 就是"CPU 一帧要 20 ms"的直接倒数；
> 2. 这 20 ms 里几乎全是**每帧把 768 个字形重新 `swash` 光栅化**（48px 物理尺寸下
>    ≈0.023 ms/字）；稳态同样的 732 条目 / 768 quad，记录+prepare+提交合计只要 **0.74 ms**；
> 3. vsync 等待是**自适应**的：CPU 一旦超过 8.33 ms，present 几乎不阻塞（等待 0.11 ms），
>    所以帧时间是"非整数倍"的 20 ms，而不是整齐的 16.67。

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

**实测**（机器空闲时的 release 构建；复跑方式见 §8）：

| 场景 | 结果 |
|---|---|
| `prewarm` 冷（105 条 32px） | 0.025 ms/字 |
| `prewarm` 命中 | 0.018 ms（48 条全命中） |
| 触发扩容的那次 `prewarm` | 3.2 ms（含该次自身 ~60 字的现场光栅化 + 新建纹理 + 拷贝） |
| `save`（105 条 / 75 KiB） | 13.2 ms |
| `load`（105 条） | 8.8 ms |
| `save`/`load`（1200 条 / 616 KiB 内容、图集 1024²） | save 18 ms（655 KiB）/ load 19.8 ms，**0 像素差** |
| `save`/`load`（**Android 模拟器**、720 条 / 828 KiB） | save → 852 KiB 文件 / load 720 条、0 跳过，**装载后绘制 0 次光栅化** |
| `compact` | 无需收缩时 ~0.01 ms；要搬条目时与条目数成正比 |
| 稳态帧（105 条已缓存） | 1.66 ms（900×600 画布，含提交） |

> 这些数字对**机器负载**敏感：同一套用例在机器忙时跑出过 save 23–36 ms、load 13–18 ms、
> prewarm 0.04–0.07 ms/字。比较时请在同一次空闲窗口里跑完整组。

两条语义层面的结论（都有用例盯着，不是"大概不会"）：

- **A/B 两套文案隔帧交替**：稳态 **0 次重光栅化**、0 次扩图 —— 热条目（上一帧还在用）
  不会被淘汰，所以来回切换不会互相踩。
- **并发**：4 线程同时对同一 `GpuContext` 做 `prewarm`/`hold`/`trim`/`stats`，
  结束后钉子归零、条目自洽（内部是一把互斥锁，并行度 1，但不会破坏状态）。

## 7. 性能建议

1. **加载阶段预光栅化**：`prewarm` 一遍加载界面/主菜单会出现的文案，之后每帧纯命中。
   放在 `app.run` 之前或首个 `prewarm` 调用里都可以（视口没建时会自动用占位分辨率）。
2. **钉住骨架字形**：标题、固定标签用 `hold`，它们既不重光栅化也不挤占预算。
3. **给个预算，但别小于工作集**：UI 文案会变的场景（数字、玩家名）设个 4–16 MiB 预算，
   图集就不会按历史峰值常驻；但预算 **< 单帧工作集**会退化成每帧重光栅化
   （实测 6.6× 帧时间，见 §6.2）。拿 `stats().used_bytes()` 当工作集，留 1.5–2× 余量。
4. **想要跨帧零重光栅化**：用固定字号 + 整数坐标 + `StableText`/`prewarm`；
   跑 `--example text_cache` 按空格暂停，`rasterizations` 不动就说明做到了。
5. **别每帧调 `entries()`**：它是 O(条目数) 的全量快照，`stats()` 才是 O(1) 聚合。
6. **量一遍再调**：`cargo run --example text_profile` 看排版/光栅化分项；
   `cargo test --release --test gpu_bench -- --ignored --test-threads=1 glyph_cache_ --nocapture`
   打印本机上的 prewarm / grow / save / load / 预算抖动 / 大图集 / 并发数字（§6.2、§6.5 的表就是它）。
7. **真窗口里复核**：`cargo run --release --example text_cache` 手动玩（`1/2/3` 切预算、
   空格暂停看 `光栅化` 是否不动、`S/L` 存取），或
   `VIREO_TEXT_CACHE_BENCH=1 cargo run --release --example text_cache`
   自动跑三档预算、每档打印一行 `ms/帧 + 光栅化次数` 后自行退出。
   Android 上同一套场景：`adb shell setprop debug.vireo.text_bench 1` 后启动
   `examples/android` 的 app，结果打到 logcat（`adb logcat -s vireoandroid`）并写进
   `files/text_bench.txt`。

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
