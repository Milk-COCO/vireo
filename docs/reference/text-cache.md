# 文字：缓存与自行管理

> 本文负责**自动行为、观测接口、以及"我要自己管"的剧本**。
> 定义与绘制见 [text-api.md](text-api.md)；字体见 [text-fonts.md](text-fonts.md)；
> 性能画像见 [text-perf.md](text-perf.md)；概念见 [text.md](text.md)。

## 0. 心智模型

文字有两层缓存，**各有各的失效原因**：

| 缓存 | 键 | 失效/回收 | 默认行为 |
|---|---|---|---|
| **排版缓存**（typeset） | `(文本, TextDef)`（字号/折行/对齐/attrs/style） | TTL 2s、条数上限 4096、GC | 自动，无需干预 |
| **字形缓存**（glyph atlas） | `(字形, 生成尺寸, 子像素档, 带宽档)` | 热/冷淘汰、预算、自动压缩 | 自动，无需干预 |

**默认配置就是安全区**：什么都不设时，排一次、画一次，稳态 0 次重光栅化
（目标场景实测 121 fps，见 [text-perf.md](text-perf.md)）。下面所有旋钮都是为了
"换场景 / 控显存 / 跨启动"这类额外目标。

`StableText` 是第三层（实例固化），它既不是排版缓存也不是字形缓存，见
[text-api.md §6](text-api.md#6-稳定句柄stabletext)。

## 1. 排版缓存

```rust
let t = gpu.text();
t.set_typeset_ttl(Some(Duration::from_secs(2))); // None = 不按时间回收
t.set_typeset_max_entries(Some(4096));           // None = 不限条数
let s = t.typeset_stats();                       // hits / misses / gc_runs / last_gc_us / total_gc_us
t.reset_typeset_stats();
t.clear_typeset_cache();                         // 立即清空；StableText 持有的槽不受影响
```

- **GC 用真实时间**（与帧率无关），两次 GC 最短间隔 250ms；软目标 512 条，超过后优先清过期项。
- **被 `StableText` 持有的槽不会被回收**（`typeset_held_count()` 可查）—— 句柄活着，排版就活着。
- `typeset_len()` 是当前条目数；`clear_resolved_glyph_cache()` / `resolved_glyph_cache_len()`
  管的是"逐字解析"（`TextPart::Glyphs` / `TextPart::Stable` 用）那一层的缓存。
- **改字体后自动重建**：`load_font` 会清空排版缓存与逐字解析缓存（fallback 可能变了）。

排版的成本量级：长串 5–30 ms，命中时接近 0。所以动态文案（每帧 `format!`）会让缓存
永远 miss —— 用 `TextPart` 分段，见 [text-api.md §4](text-api.md#4-分段文字与-hud-行)。

## 2. 字形缓存

入口 `gpu.text().glyphs()`，返回 `GlyphCache<'_>`（借用 `GpuContext`，不能存到别处）。

底层是**两张图集**：R8（覆盖度 + 精确距离场，1 B/px）与 RGBA8（彩色字形，4 B/px）。
边长从 256 起按需 ×2，直到 `max_texture_dimension_2d`。

### 2.1 观测

```rust
let g = gpu.text().glyphs();
let s = g.stats();                       // O(1) 聚合，可每帧调
println!("{} 条 / 占用 {} KiB / 纹理 {} KiB / 峰值 {} KiB / 压缩 {}",
    s.entries(), s.used_bytes() / 1024, s.texture_bytes() / 1024,
    s.peak_used_bytes / 1024, s.compacts);
for e in g.entries() { /* O(n) 快照：atlas/content/尺寸/坐标/字节/pinned/in_use/key */ }
```

`GlyphCacheStats` 字段：`mask_entries` / `color_entries` / `pinned_entries` /
`mask_used_bytes` / `color_used_bytes` / `mask_texture_bytes` / `color_texture_bytes` /
`retention_budget_bytes` / `hard_budget_bytes` / `effective_budget_bytes` / `auto_compact` /
`rasterizations` / `evictions` / `grows` / `compacts` / `trims` / `budget_warnings` /
`peak_used_bytes`；便捷方法 `entries()` / `used_bytes()` / `texture_bytes()`。

约定：`*_used_bytes` 是**条目面积累计**（不含空洞）；`*_texture_bytes` 是**纹理容量**
（`边长² × 通道数`，按 2 的幂翻倍，所以通常比占用大一圈）。

另有一层更粗的图集视角：`gpu.text().atlas_stats()` → `mask_size` / `color_size` /
`mask_bytes` / `color_bytes` / `mask_entries` / `color_entries`（+ `total_bytes()`）。

### 2.2 容量：扩、淘汰、缩

- **扩**：放不下就 ×2（拷贝搬运旧条目，**不重新光栅化**）。
- **淘汰**：分两档 —— 先扔「本帧与上一帧都没用到」的冷条目，还不够才动热条目。
- **缩（自动压缩，默认开）**：淘汰只释放条目，纹理只涨不缩，所以利用率长期偏低时
  自动缩回「刚好装下」的 2 的幂纹理（拷贝、不重光栅化、像素不变）。

自动压缩的触发与代价：

| 项 | 值 |
|---|---|
| 触发 | 任一张图利用率 **< 25% 持续 60 帧**，且距上次压缩/扩容 ≥ 120 帧 |
| 搬运成本 | 实测 371 KiB / 720 条 → **1.3 ms**（4096² → 1024²） |
| 自动模式上限 | 只搬 `live ≤ 512 KiB`（最坏约 2 ms） |
| 失败退避 | 打包排不下 → 600 帧后再试（`stats().compacts` 可确认） |

手动入口：`set_auto_compact(bool)` / `compact(&device, &queue)`（放两帧之间调，会改条目坐标）。

### 2.3 预算：保留 vs 硬

| 模式 | 入口 | 淘汰谁 | 会掉帧？ | 用途 |
|---|---|---|---|---|
| 无预算（默认） | 两个都 `None` | 不淘汰 | 否 | 简单场景 |
| **保留预算** | `set_retention_budget(Some(x))` | 只淘汰「**≥120 帧没用到**」的条目 | **否** | 限制"历史积压"（换场景/改字号留下的冷字形） |
| **硬预算** | `set_hard_budget(Some(x))` | 冷条目扔完再动热条目，直到占用 ≤ x | **可能** | 真正钉死内存上限 |

两个 setter 互斥，后设置者生效；`effective_budget_bytes()` 是当前实际执行值。

**为什么保留预算不会掉帧**：它只淘汰"1 秒没碰过"的条目，当前工作集与间歇使用的字形
（只在某些帧出现的数字）都不动。实测同一个 HUD（20 条标签 20–28px、工作集 27 KiB）：

| 配置 | 帧耗时 | 光栅化 |
|---|---|---|
| 硬 6 KiB（25% 工作集） | 2.18 ms | **50 次/帧** |
| **软 6 KiB（同一个数）** | **0.75 ms** | **0 次/帧** |
| 软 54 KiB（200%） | 0.70 ms | 0 次/帧 |

**硬预算低于单帧工作集必然掉帧**（详见 [text-perf.md](text-perf.md#预算语义的代价)）。
`stats().peak_used_bytes` 就是单帧工作集的实测值；硬预算低于它时 `set_hard_budget`
会 `log::warn!` 一次。

### 2.4 预光栅化与钉子

```rust
let g = gpu.text().glyphs();
let scale = win.layout_scale() as f32;

// 只填图集，不产生任何可见输出；返回本次新光栅化的条目数
let new = g.prewarm(&gpu.device, &gpu.queue, "加载中…", &def, scale);

// 填图集 + 钉住：句柄活着期间这批字形不会被 LRU/预算淘汰
let hold = g.hold(&gpu.device, &gpu.queue, "标题", &def, scale);
drop(hold); // 解钉
```

- 同一批字形被多个句柄钉住是**引用计数**语义，最后一个释放才解钉；
- 钉子保护的是**光栅化条目**，不是排版结果；"排版也别重排"用 `StableText`；
- 预光栅化命中的前提是**位置的小数部分落在同一子像素档**（最多 4×4 档/字号）。整数
  坐标画字天然落在 `Zero` 档，与 `prewarm` 一致；`x = 12.37` 这类位置是另一个档。

### 2.5 性能哨兵

稳态下每帧仍在重光栅化时（连续 30 帧 `rasterizations` 增长），库在 debug 构建或
`VIREO_TEXT_STRICT=1` 下 `warn` 一次，并给出三条排查方向：硬预算过小 / 字号连续动画 /
动态文案整串 `format!`。所有诊断都走 `log::warn!`，**应用要装 logger 才看得到**：

```rust
vireo::init_logger();     // env_logger；RUST_LOG=warn 即可看到
```

## 3. 自动还是自己管

| 目标 | 默认行为够吗 | 要做什么 |
|---|---|---|
| 普通 UI / HUD | 够 | 什么都不做 |
| 首帧卡一下（PSO/图集初始化） | 不够 | 启动时 `gpu.preheat_text(..)`；加载界面 `prewarm` |
| 换场景后显存不降 | 部分 | 默认自动压缩已处理；大缓存手动 `compact()` |
| 显存要钉死上限 | 不够 | `set_hard_budget`（接受掉帧）或保留预算 + 内容约束 |
| 冷启动太慢 | 不够 | 跨进程 `save`/`load` |
| 大段静态文字 CPU 太高 | 不够 | `StableText`（见 [text-api.md §6](text-api.md#6-稳定句柄stabletext)） |
| 动态数字每帧重排 | 不够 | `TextPart` 分段（见 [text-api.md §4](text-api.md#4-分段文字与-hud-行)） |

### 剧本 A：加载界面把该付的成本付掉

```rust
// 1) 字体/图集的懒初始化（用单字符跑一次 prepare；需 renderer 已 resize 过）
gpu.preheat_text(&gpu.device, &gpu.queue, w, h);
// 2) 会出现的文案
for (text, def) in catalog {
    gpu.text().glyphs().prewarm(&gpu.device, &gpu.queue, text, &def, scale);
}
// 3) 固定不变的大段文字：连实例一起烘
for block in static_blocks {
    block.bake(&gpu, scale);
}
```

### 剧本 B：限制显存但不掉帧

```rust
let g = gpu.text().glyphs();
g.set_retention_budget(Some(4 << 20)); // 只清历史积压
// 换场景后可选：立刻还回去（放在两帧之间）
g.compact(&gpu.device, &gpu.queue);
```

### 剧本 C：真正钉死上限

```rust
g.set_hard_budget(Some(2 << 20));
```

低于工作集时每帧都会重光栅化 —— **这是物理限制**，不是实现缺陷：字形在当前帧必须存在。
要么扩内容预算，要么减少同屏字形。

### 剧本 D：跨启动复用

见 §4。

### 剧本 E：自己接管淘汰节奏

```rust
g.set_auto_compact(false);            // 关掉自动缩
g.set_retention_budget(None);         // 关掉自动淘汰
// 自己在"回主菜单"这类时机：
g.trim();                             // 按预算淘汰（未设预算 = 不做事）
g.clear_unpinned();                   // 清掉未钉住的
g.compact(&gpu.device, &gpu.queue);   // 缩纹理
```

### 并发

`GpuContext` 内部是一把互斥锁，多个线程同时 `prewarm`/`hold`/`trim`/`stats` 不会破坏状态，
但**并行度是 1**：不要指望靠多线程扩大文字准备吞吐。

## 4. 跨进程持久化

```rust
// 退出前 / 加载界面结束时
let n = gpu.text().glyphs().save(&gpu.device, &gpu.queue, "cache/glyphs.vrtx")?;

// 下次启动、刚建好上下文时（图集必须为空）
gpu.text().glyphs().load(&gpu.device, &gpu.queue, "cache/glyphs.vrtx")?;
```

- 格式 `VRTX` v1：字体表（postscript 名 + 字体数据**采样摘要**）+ 每条字形的
  `(键, 尺寸, bearing, 像素)`；
- **字体靠摘要认**：`fontdb::ID` 是进程内 slotmap 键，跨启动必然不同。装载时按名字找候选面
  再验摘要，认不出的条目**跳过**（返回 `Ok(跳过后的条数)`，不是错误）；
- **装载要求图集为空**（`glyphs().clear()` 之后，或刚启动还没画字），否则
  `ErrorKind::InvalidInput`；
- 装载按面积重新打包（`etagere` 不能把块固定在指定坐标），顺带得到无碎片图集；
- 文件体积 ≈ 条目像素之和（R8 `w×h`、彩色 `w×h×4`），无压缩；
- 两端都批量提交（保存只读回"用到的矩形"；装载一个 staging buffer + 一次 `submit`）；
  一次 `poll(Wait)` 不可避免，它会把队列里已提交的绘制也排空；
- 魔数/版本不符、文件截断 → `ErrorKind::InvalidData`，图集保持原样。

**不含排版缓存**：跨启动省掉排版成本需要自己保存文案并用 `StableText` 重建。

## 5. 诊断开关

| 开关 | 作用 |
|---|---|
| `vireo::init_logger()` + `RUST_LOG=warn` | 看到所有 `log::warn!`（预算不足、哨兵、带宽截断） |
| `VIREO_TEXT_STRICT=1` | release 构建里也开启性能哨兵 |
| `VIREO_TEXT_DEBUG=1` | 逐字形的调试输出（噪声很大，排查"这个字怎么画的"时才开） |
| `stats().rasterizations` 的**增量** | 稳态是否还在重光栅化（应为 0） |
| `typeset_stats()` | 排版命中率；`misses` 每帧涨说明文案每帧都变 |

## 6. 常见现象 → 原因

| 现象 | 最可能的原因 | 怎么办 |
|---|---|---|
| 稳态 `rasterizations` 每帧涨 | 硬预算 < 工作集；字号连续动画；动态串整串 `format!` | 换保留预算；量化字号；`TextPart` 分段 |
| 换场景后显存不降 | 纹理只由扩容抬升 | 等自动压缩（默认开）；或手动 `compact()` |
| 显存一直涨 | 没有淘汰（无预算）且内容持续变多 | 设保留预算 + 自动压缩 |
| 首帧卡顿 | PSO/字体/图集懒初始化 | `preheat_text` + 加载期 `prewarm` |
| 中文显示成方块 | 字体没有对应字形且无 fallback | 见 [text-fonts.md](text-fonts.md) |
| `hold` 之后显存不降 | 句柄没释放 | `drop(hold)`；`stats().pinned_entries` 查 |
| 保存后再装载"少了几条" | 换了字体文件（摘要不匹配）| 正常行为；见 §4 |
| 滚动大段文字 CPU 高 | 每帧逐字形 prepare | `StableText`（实例固化） |

## 7. 限制

- **交替内容超过预算会互相踩**：保留预算只保证"不淘汰最近 120 帧用过的字"。两套内容
  交替使用、加起来远超预算时，它们会互为冷条目 —— 这是"内容装不下"，不是 bug。
- **钉子不会自动过期**：`hold` 句柄忘 drop 就永远占着预算。
- **图集有上限**：边长最多 `max_texture_dimension_2d`（常见 8192/16384），到顶后只能淘汰。
- **持久化不含排版**、不含 `StableText` 的实例块。
