# 文字：性能画像与自查

> 本文负责**实测数字、瓶颈归因、复现命令**。缓存机制见 [text-cache.md](text-cache.md)，
> 定义见 [text-api.md](text-api.md)。

## 目标与判据

目标是**稳态 120 fps**：`8.33 ms` 预算里，文字还能装多少。判据是两条曲线：

- **CPU**：`DrawReport::timings.encode_secs`（记录 + prepare + 提交）；
- **GPU**：每帧 `device.poll(Wait)` 后的完整帧时间。

测量方法统一为：目标尺寸画布、空 batch 帧做**同一画布**基线、3 轮取最小值。

## 实测：CPU 与 GPU

4096² 画布、16px、缓存全热、500–2000 段不重复文案：

| 文本量 | CPU 净成本 | 每 quad | GPU 净成本 | 每 quad |
|---|---|---|---|---|
| 6 000 quad | 0.35–0.49 ms | 0.06–0.08 µs | **1.78–2.15 ms** | ~0.30 µs |
| 12 000 quad | 0.84–2.17 ms | 0.07–0.18 µs | **3.9–5.3 ms** | ~0.35 µs |
| 24 000 quad | 5.5–16 ms | 0.23–0.68 µs | **8.5–15.8 ms** | ~0.55 µs |

**结论一：GPU ≥ CPU，且一屏文字里 CPU 差 4–5 倍。**

| 文本量 | CPU 净 | GPU 净 | GPU / CPU |
|---|---|---|---|
| 6 000 quad | 0.35–0.49 ms | 1.78–2.15 ms | **4.5×** |
| 12 000 quad | 0.84–2.17 ms | 3.9–5.3 ms | 2.4–4.6× |
| 24 000 quad（不同字形） | 14.9–16.5 ms | 17.2–17.8 ms | ~1.1× |
| 24 000 quad（60 种字形） | 8.3–8.4 ms | 8.7–9.6 ms | ~1.1× |

⇒ **继续优化 CPU 路径不会改善端到端帧时间**，除非把 CPU 那一列压到 GPU 以下 —— 而它已经在
那里了（除 24k 档）。

## 结论二：高文本量由「字形种类」主导

同一布局、同样 24 000 quad / 2 000 条目，只改字形种类：

| 不同字形数 | GPU 净成本 |
|---|---|
| 24 000 种（每条文案都不重样） | **15.5–15.8 ms** |
| 6 000 种 | 10.8–17.2 ms |
| 600 种 | 12.1–12.8 ms |
| **60 种**（5 段文案复用） | **5.7–8.7 ms** |

差别来自**图集大小**：60 种字形 ⇒ 256²；24 000 种 ⇒ 4096²（16 MB），随机采样打穿纹理缓存。
真实 UI 的同屏字形种类通常只有几百种。

## 结论三：`8.33 ms` 能画多少

- 真实内容（≤600 种字形）：约 **1.5 万–2 万 quad**（先撞 GPU）；
- 全不重复字形：约 **1 万 quad**。

示例场景（768 quad、DPI 2）编码 0.7–0.9 ms，**10 倍以上余量**；关 vsync 实测
1 000–2 000 fps。

## 结论四：条目数几乎免费

固定 24 000 quad，2 000 / 800 / 200 条目的 CPU 净成本是 13.6 / 11.5 / 9.9 ms
（约 1.3 µs/条目）。想省就少 `draw_text` 几次、拼成长串。

## 实例固化（`StableText`）的收益

12 000 字的块（4096×1000 画布、`max_width` 折行）：

| 路径 | 帧成本 |
|---|---|
| 逐字形 prepare（`draw_text`） | **2.0–2.3 ms** |
| `StableText` 复用烘焙实例 | **0.15–0.26 ms** |

约 **8–9×**。注意：测这个必须**预建 batch 且不要 `read_pixels()`** —— 4096×1000 回读
约 8 ms，会把结论反过来。

## 预算语义的代价

硬预算小于单帧工作集时，每帧都要重光栅化。同一场景（60 段 × 12 字、24px、每帧全画，
工作集 371 KiB）三处实测：

| 环境 | 工作集 | 硬预算 < 工作集 | 预算够 | 倍数 |
|---|---|---|---|---|
| 桌面无头（Win11/DX12, release） | 371 KiB | 8.7–13.8 ms/帧、**479 次/帧**光栅化 | 0.4–0.9 ms/帧、0 次 | ~10–20× |
| Windows 真窗口（DPI 2、120 Hz vsync） | 1518 KiB | **18.5–20.0 ms/帧（50–54 fps）**、768 次/帧 | 8.27 ms（120.8 fps）、0 次 | **2.2–2.4×** |
| Android 模拟器（dpi 1.5、60 Hz） | 828 KiB | **134.6–137.7 ms/帧（7.3–7.4 fps）**、635 次/帧 | 16.45–16.50 ms（60.7 fps）、0 次 | **8.1–8.4×** |

真窗口倍数小是因为基线被 vsync 钉在 8.3 ms —— 它已经掉到 50 fps 了。工作集随 `scale²`
增长：200% DPI 下同样文案是 1.5 MiB，不是 371 KiB。

## 自查流程：先分清 CPU / GPU

1. 看 `report.timings.encode_secs`（CPU）与整帧时间；两者差距大 ⇒ 在等 GPU/vsync。
2. 跑归因用例拿到双列数字：

   ```bash
   cargo test --release --test gpu_bench -- --ignored --test-threads=1 \
     text_path_quad_attribution --nocapture
   ```

3. 按症状对症：

| 症状 | 先砍这个 | 手段 |
|---|---|---|
| CPU 侧高（`encode_secs`） | 条目数与每帧重光栅化 | 拼成长串少 `draw_text`；固定字号 + `prewarm`/`StableText`；`rasterizations` 稳态应为 0 |
| GPU 侧高 | **同屏字形种类**（图集大小） | 少用生僻字/多语言混排；同屏种类 24 000 → 60，24 000 quad 的 GPU 成本 15.5 → 5.7 ms |
| GPU 侧高、种类也不多 | **overdraw**（文字叠文字） | 别大面积重叠；`text_clip`/`scissor` 裁掉不可见部分；字号别远大于显示尺寸 |

## 已经修掉的真实问题

1. **每字形查环境变量**：`prepare_glyph` 里曾有 `std::env::var_os("VIREO_TEXT_DEBUG")`，
   每次拿 env 锁 + 平台查询（Windows 上 `GetEnvironmentVariableW`）≈0.5 µs/字形 ⇒
   一帧几千字形就是几毫秒。改成 `OnceLock` 缓存一次：6000 quad 从 **4.13 → 2.46 ms**。
2. **字号没有量化**：未样式文本的缓存键含精确字号，"字号连续动画"每帧都是新键 ⇒ 整段重新
   光栅化（12 字/帧、1.03 ms）。现在光栅化键的字号量化到 **1/8 px**，同一动画降到 ~3 次/帧。
3. **字形图集从不淘汰**：帧代数没推进，`try_allocate` 永远认为"全都在用" ⇒ 只扩不淘。
   现在每帧推进，淘汰分冷/热两档。
4. **扩图集重光栅化全部字形**：改成 `copy_texture_to_texture` 原坐标搬运。
5. **热路径 LRU 拼接**：`get_mut` 每次命中都做一次双向链表拼接，改成 `peek_mut`：
   重复字形场景 24 000 quad **6.68 → 3.18 ms**。
6. **样式/几何描边在 resolved 路径丢失**：参数被写死成 0/None；现在两条路径逐像素一致。

## 复现命令

```bash
# 吞吐上限 + CPU/GPU 归因（要 GPU）
cargo test --release --test gpu_bench -- --ignored --test-threads=1 text_path_ --nocapture

# 字形缓存的 prewarm/grow/save/load/预算抖动/大图集/并发
cargo test --release --test gpu_bench -- --ignored --test-threads=1 glyph_cache --nocapture

# 实例固化：像素一致性 / 复用与失效 / 预烘焙 / 成本
cargo test --release --test gpu_bench -- --ignored --test-threads=1 stable_bake --nocapture
```

真窗口里复核：

```bash
VIREO_TEXT_CACHE_BENCH=1 cargo run --release --example text_cache
```

它会自动跑三档预算语义（软 / 硬 / 无），每档打印 `ms/帧 + 光栅化次数` 后自行退出。
Android 上：`adb shell setprop debug.vireo.text_bench 1` 后启动 `examples/android`，
结果打到 logcat（`adb logcat -s vireoandroid`）并写进 `files/text_bench.txt`。

## 已知瓶颈（按性质分类）

- **GPU 采样/填充**：一屏文字的可见成本主要在这里，由"内容长什么样"决定（字形种类、
  overdraw），库里没有更多可优化点。
- **每条目的 1.3 µs**：`TypesetKey` 构造与 `AreaMeta` 记账。分段比逐条便宜。
- **`StableText` 相位固定**：位置带小数时抗锯齿相位不随位置变（见 [text-api.md §6](text-api.md#6-稳定句柄stabletext)）。
- **持久化的一次设备等待**：`save`/`load` 各一次 `poll(Wait)`，会把已提交的绘制也排空。
