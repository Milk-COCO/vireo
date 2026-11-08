# Changelog / 更新日志

## [0.2.0] — 未发布 / unreleased

### Changed / 变更

- **字形缓存的预算拆成两个旋钮**（原来是 `set_budget` 一个，语义含混）：
  - `set_retention_budget(..)`（**保留预算**，推荐）：只淘汰「≥120 帧没用到」的条目，
    当前工作集与间歇使用的字形都不动 ⇒ **永不掉帧**；它限制的是"历史积压"
    （换场景/改字号留下的冷字形），不是显存上限；
  - `set_hard_budget(..)`：严格上限，低于单帧工作集时会每帧重光栅化（掉帧），
    设置时会 `log::warn!` 一次；两个 setter 互斥，后设置者生效。
- **`GlyphCacheStats` 字段同步调整**：`budget_bytes` → `retention_budget_bytes`，新增
  `hard_budget_bytes` / `effective_budget_bytes` / `compacts` / `auto_compact`；
  `grows` 现在只统计扩容（之前 `compact` 会误加）。
- **文字 API 归一到一个门面**：新增 `GpuContext::text() -> &TextSystem` 与
  `TextSystem`（字体 / 排版缓存 / 字形缓存 / 图集统计 / 度量 / 句柄 / 预热 / 渲染器的
  统一入口）。`GpuContext::text_ctx` 字段改为内部实现（用户不再需要碰互斥锁）。
- **BREAKING：`GpuContext` 上不再转发任何文字方法，只留 `text()`**：删除
  `measure_text` / `measure_text_visual` / `text_atlas_stats` / `load_font` /
  `make_stable_text` / `preheat_text` / `clear_glyph_cache` / `glyph_cache_len`，以及
  9 个 `typeset_*` 旋钮，全部改用 `gpu.text().<对应方法>()`（`measure_text`→`measure`、
  `measure_text_visual`→`measure_visual`、`text_atlas_stats`→`atlas_stats`、
  `make_stable_text`→`stable`、`preheat_text`→`preheat`、`clear_glyph_cache`→
  `clear_resolved_glyph_cache`、`glyph_cache_len`→`resolved_glyph_cache_len`）。
  理由：两套入口只会让"该改哪个"变模糊，且转发层的文档/参数漂移没有收益。
- **BREAKING：资源加载统一成「来源 + 一个入口」**：新增
  [`Source`](src/asset.rs)（`Asset(name)` / `Path(path)` / `Bytes(data)`，显式指定、
  不自动回退），每个资源只保留一个加载方法：

  | 资源 | 之前 | 现在 |
  |---|---|---|
  | 贴图 | `App::load_texture(path)` + `App::load_texture_asset(name)` | `App::load_texture(Source)` |
  | 字体 | `TextSystem::load_font_file(path)` + `App::load_font_asset(name)` | `App::load_font(Source)`（无 `App` 时仍用 `Texture`/`TextSystem::load_font(bytes)`） |
  | 窗口图标 | `WindowDesc::icon_from_path(path)` + `icon_from_asset(name)` | `WindowDesc::icon_from(Source)` |

  同时 `Assets` 只有一条构造路径（`Assets::local`），修掉 `icon_from_asset` 曾经自己
  拼 `Assets::new(current_root())` 绕过 `App::assets()` 的分叉。
  理由：加载动作原先是"资源类型 × 来源"的笛卡尔积，且字体那条裂在两个类型上 ——
  用户被迫记住哪个 API 属于谁；现在只需记住"哪种来源"。
- **BREAKING：排版缓存旋钮只留在 `gpu.text()` 上**：删除 `GpuContext` 上的
  `set_typeset_ttl` / `typeset_ttl` / `set_typeset_max_entries` / `typeset_max_entries` /
  `clear_typeset_cache` / `typeset_len` / `typeset_held_count` / `typeset_stats` /
  `reset_typeset_stats` 九个纯转发方法，改用 `gpu.text().<同名方法>()`。
  理由：缓存调优是文字系统的职责，两套入口只会让"该改哪个"变模糊。
- **「shape 缓存」改名「typeset / 排版缓存」**：文字侧统一用 `typeset`，因为 `shape`
  在本库里指几何图形、`layout` 指 UI 布局。`set_shape_cache_ttl` / `shape_cache_len` /
  `clear_shape_cache` / `shape_cache_stats` 等旧名**直接删除**（0.2.0 是 breaking 版本，
  不保留兼容壳）。
  示例 `text_shape_cache` 同步改名为 `text_typeset_cache`。
- **移除 MSDF 可选路径**：只保留「精确按尺寸 SDF + 尺寸阶梯」，`TextSdfMode` /
  `TextDef.sdf_mode` / `ContentType::Msdf` / 独立 MSDF 图集一并删除。评估结论：MSDF 面积比
  精确 SDF 胖 **+1.5%~+4%**，但尖角并不更锐；每像素 4B 再加 range 补边，导致 **10–20×**
  内存；满屏实测 **821ms/帧**；而「改字号不重建」已由尺寸阶梯以约 1/10 内存实现。
  详见 `docs/reference/text-style.md` §5。
- **BREAKING：库自己的错误统一为 `VireoError`**：材质 / 计算管线创建原本返回
  `Result<_, String>`（`App::material` 与 `GpuContext::create_material*` /
  `create_compute_pipeline`），现在返回 `Result<_, VireoError>`，诊断文本放在新增的
  `VireoError::Material(..)` 里。`ExternalError` / `NotSupportedError` 只继续用于透传
  winit 语义的窗口操作（`set_cursor_grab` / `inner_position` 等）。
- **BREAKING：加载失败不再静默**：`App::load_texture` 现在返回
  `Result<_, VireoError>`（读不到 / 解码失败），`Texture::from_file` / `from_bytes`
  同理；「坏图也要继续画」变成显式 opt-in ——
  `App::load_texture_or_placeholder(..)` → `Texture::placeholder(&gpu)`（棋盘占位图，
  `Texture::is_placeholder()` 可自查）。`Texture::from_rgba` 的非法参数（宽高为 0、
  缓冲区太短）从"静默给占位图"改成 `assert!`：那是调用方编程错误，不该被吞。
  `Texture::wrap(..)` 成为外部构造 `Texture` 的唯一入口（结构体仍可读字段，
  但不再允许字面量构造）。
- **窗口图标：`icon_from` 保留最佳努力，新增 `try_icon_from`**：前者失败打 `log::warn`
  并保持原图标（builder 链式调用不便返回错误），后者返回 `Result<Self, VireoError>`
  供程序化判断 —— 两者共用同一份解码逻辑。
- **失败契约写进文档**：`error.rs` 模块文档写明「默认返回 `Result`，静默降级必须由
  调用方显式选择（名字里写明）」及其四种场景，避免以后按资源类型随手选。

### Fixed / 修复

- **`StableText` 画不出样式与几何描边**：resolved 路径把样式的 `style_layers` /
  `style_above_mask` / `style_reach` / `vector_stroke` 写死成 0/None，且句柄不保留
  `TextDef`，于是 `b.text_stable` 的描边、投影、发光、几何描边全部丢失（`b.text` 正常）。
  现在句柄保留完整定义，这些参数由 prepare 统一算好传入，两条路径逐像素一致。
- **resolved 路径的层绘制顺序**：它每个字形各用一组 `QuadBuckets` 并逐字形 flush，
  相邻字形的半透明投影/发光按"逐字形"而不是"逐层"合成（后面的字形会压住前一个的层）。
  改成整次 prepare 共用一组桶、按层跨字形排序，与普通路径一致。
- **字形图集从不淘汰**：`InnerAtlas::trim()`（推进帧代数）从来没有被调用过，导致
  `try_allocate` 永远认为「所有条目本帧都在用」→ 图集只扩不淘，显存一路涨到最大纹理。
  现在每帧 `advance_frame` 推进会话代数，淘汰分两档：**先扔冷条目**（本帧与上一帧都没用到），
  不够才扩图集，图集到顶后才动「上一帧还在用」的热条目。
- **扩图集不再重新光栅化全部字形**：`grow()` 原来重建纹理后把所有字形重光一遍（几十上百个
  字形，毫秒级可见卡顿）；现在用 `copy_texture_to_texture` 原坐标搬运（`etagere::grow` 不移动
  已分配的块）。代价是失去「重光栅化时顺手校验条目」的自愈能力，换字体后用 `clear()` 处理。
- **距离场带宽随字号缩放**：带宽档（4/8/16/32/64）改为按 44px 参考字号解释、再按实际字号换算。
  之前是绝对设备像素，200% 缩放下 32 逻辑像素以上的描边会"再拉粗没变化"。
- **描边宽度的两处跳变**：`band_q` 量化上限原来钳在 256（= band 32），跨到更大档时距离翻倍、
  视觉厚度减半；现按 `BAND_GEN_MAX * 8` 量化。
- **饱和区不再被当成"内部"**：当样式阈值 ≥ 带宽（大描边 / 高 DPI）时，距离场饱和值 ≈ 阈值会让
  `coverage = 0.5`，整块 padded quad 变成半透明矩形；现在饱和区直接按"远处"返回。
- **窗口边缘的文字不再被压扁**：quad 被 `bounds` 裁剪后，UV 推进量按裁剪后的 texel 尺寸走
  （之前仍按整条条目推进，整条字形被压进更小的 quad）。
- **层偏移（投影/发光）**：偏移换算用对图集尺寸；并折入尺寸阶梯的 `quad_scale`（之前少 4%）。
- **尺寸阶梯的 `band` 按生成空间推导**：`band_gen = band_for_reach(reach * 生成字号 / 目标字号)`，
  否则外扩在生成空间里不够，描边会被截断。
- 示例：`←→` 改宽度时按住会加速（0.5/120ms → 1.0/60ms → 2.0/30ms），从 2 拉到 30 约 1 秒；
  左上角 HUD 显示帧耗时；`VIREO_FPS_LOG=1` 每秒打印 fps，`VIREO_DEMO_GROUP=N` 指定起始组。

### Added / 新增

- **弧线描边 `draw_arc` / `Shape::Arc`**：只描给定的角度范围、**不含**圆心到两端的半径
  （对比 `draw_sector_outline` 的描边扇形、`draw_circle_outline` 的整环）。沿弧采样折线，
  默认走折线 SDF 距离场（边缘抗锯齿与圆环一致），`clear_sdf_feather()` 后退化为几何模板；
  整圈（`0..=TAU`）与 `draw_circle_outline` 逐像素一致。两端是平头、不闭合。
- **BREAKING：`arc` 回归曲线含义，填充的弧改名 `sector`**：`draw_arc` → `draw_sector`、
  `draw_arc_outline` → `draw_sector_outline`、`Shape::Arc` → `Shape::Sector`、
  `Shape::ArcOutline` → `Shape::SectorOutline`（`DrawBatch::arc` / `arc_outline` 同样改为
  `sector` / `sector_outline`，另加 `DrawBatch::arc` 画弧线描边）。改名理由：`arc` 在图形学
  里指曲线，填充的弧其实是扇形；旧名字让 `draw_arc_outline` 看起来像"描一条弧"。
- **文字参考文档按主题分册**：`docs/reference/text.md` 改为索引（三层模型 / 术语 / 学习路径），
  内容拆成可单独阅读的几篇 —— `text-api.md`（`TextDef` / `TextOverride` / `TextPart` /
  HUD 分段 / 绘制入口 / 裁剪与变换 / 度量 / `StableText`）、`text-cache.md`（两层缓存的
  自动行为、预算、自动压缩、钉子、预光栅化、跨进程持久化、诊断与常见现象）、
  `text-fonts.md`（系统字体 / 加载 / family / Android 白名单 / 字体与持久化）、
  `text-perf.md`（CPU/GPU 双列实测、吞吐上限、自查流程、复现命令）。
- **`StableText` 实例固化**：句柄除了排版结果，还缓存**烘好的渲染实例** —— 首次绘制
  （或 `bake()` 预烘焙）之后，每帧只把实例拷进顶点数组并打几个补丁（整数位移/变换槽/
  颜色/UV/样式下标），不再逐字形走缓存查找与图元拼装。实测 12 000 字的块
  **2.0 ms → 0.24 ms/帧**。
  - 实例是**本地坐标**，绘制位置由一次性的平移变换槽给：**任意位置（含小数）都不重烘**，
    滚动/拖动免费。
  - 失效自动判定：DPI、`text_clip`、变换模式、图集布局（扩/缩/淘汰/清空/装载）变化；
    `bakes()` 可观测、`invalidate_bake()` 可强制重烘。
  - `StableText::bake(dev, queue, scale)` 供加载阶段提前付掉首次成本。
  - 位置落在物理像素网格上时与普通绘制**逐像素一致**（无样式 / 距离场样式 / 几何描边
    三种都有用例）；带小数时绘制位置差 ≤1 物理像素，因为抗锯齿相位固定在烘焙时。
- **用户可管的字形缓存**：`gpu.text().glyphs()` 返回 `GlyphCache<'_>`，把「字形画一次就别再画」
  和「显存别失控」这两件事交给用户：
  - **保留预算** `set_retention_budget(Some(bytes))`：只淘汰「≥120 帧没用到」的条目，
    永不掉帧；**硬预算** `set_hard_budget(Some(bytes))`：严格上限，可能掉帧（会 warn）。
  - **自动压缩**（默认开，`set_auto_compact`）：图集利用率持续 <25% 达 60 帧就自动缩到
    「刚好装下」（拷贝搬运、不重新光栅化、像素不变）；只搬 ≤512 KiB（实测 371 KiB → 1.3 ms），
    更大的缓存用 `compact()` 手动做。
  - **预光栅化** `prewarm(dev, queue, text, &def, scale)`：加载阶段把文案填进图集，
    不产生可见输出，返回新光栅化的条目数。
  - **钉子** `hold(..) -> GlyphHold`：先 `prewarm` 再钉住，句柄活着期间不被淘汰；
    引用计数语义，多个句柄钉同一批字形，最后一个释放才解钉。
  - **回收** `trim()` / `clear_unpinned()` / `clear()` / `compact()`：`compact` 把条目
    拷贝重排到更小的纹理（不重新光栅化），把显存真正还回去。
  - **观测** `stats()`（O(1) 聚合，0.05–0.10 µs）/ `entries()`（逐条快照，含 `content` 是
    `Mask`/`Sdf`/`Color`、`pinned`、`in_use`、键摘要）。
  - **性能哨兵**：稳态仍在每帧重光栅化时（连续 30 帧），debug 构建或 `VIREO_TEXT_STRICT=1`
    下 `warn` 一次，并给出三条排查方向（硬预算过小 / 字号连续动画 / 动态文案整串 format）。
- **字形缓存跨进程持久化**：`glyphs().save(dev, queue, path)` /
  `glyphs().load(dev, queue, path)`，格式 `VRTX` v1。
  - 字体身份用「postscript 名 + 字体数据摘要」而不是 `fontdb::ID`（那是进程内 slotmap 键）；
    认不出的字体整条跳过，不会画错字。
  - 装载要求图集为空，会按面积重新打包（`etagere` 无法把块固定在指定坐标），
    顺带得到无碎片图集；坏魔数/坏版本/截断文件返回 `InvalidData`。
  - GPU 用例验证：A 进程 `save` → **另一个** `GpuContext`（独立 `FontSystem`）`load` 后
    绘制，`rasterizations == 0` 且像素逐点一致。
- **文字距离场「尺寸阶梯」**：带样式文字的精确 SDF 现在按 1.25× 阶梯（`12…256`）取
  「不小于目标字号」的一档生成，档内共用一份 R8 距离场并缩放到目标字号。
  于是**同一档内改字号不再重新栅格化**（例如 40↔44px），带宽档继续负责「同档内改宽度
  不重建」。实测轮廓与覆盖度文字逐行左边缘差 ≤2px 达标率 100%（44/30/71px）。

- **文本样式（距离场图层栈）**：`TextDef.style` 新增 `TextStyle` —— 描边 / 多层描边 / 软投影 /
  发光 / 内描边 / 内阴影 / 自定义填充色，全部由**同一个原语**表达：对字形的**有符号距离场**
  取带软边的阈值。新增 `TextStyle` / `TextLayer` / `TextPaint` / `TextBlend`。
  - 字形表示升级：带样式的字形从**字体轮廓**（`skrifa`）生成**精确距离场**（线/二次/三次
    贝塞尔的最近点 Newton + 按段 bbox 剪枝），与覆盖度共用同一张 R8 图集（`content_type`
    区分），**无样式文本零改动、零额外开销**。
  - 软边用误差函数：距离场下"高斯模糊过的边缘"精确是 erf，所以**软投影/发光一次采样**，
    不需要多 tap。
  - 单位是像素意图（裸值/`Dp` 逻辑像素、`Px` 物理像素）⇒ **DPI 变化时描边视觉宽度不变**；
    `measure_text` 语义不变（样式不参与排版），新增 `measure_text_visual` 给出含溢出的视觉外框。
  - **填充**：纯色、**渐变**（`TextPaint::Gradient` + `TextGradient::in_rect`）、**纹理**
    （`TextPaint::Texture { uv, rect }` / `TextPaint::texture_in_rect`）；渐变与纹理都靠
    `rect` 决定跨满哪个页面矩形，不给则逐字形。
  - **彩色字形（COLRv1 emoji）也能描边/投影**：从光栅化后的 alpha 反推距离场（8SSEDT），
    同一次绘制里发两个 quad —— 只画图层的距离场垫在底下、彩色字形盖在上面，emoji 保留
    自己的颜色（此时 `style.fill` 被忽略）。
  - 降级（不 panic）：图层 > 8 截断 + warn；外扩超过带宽上限（相对 44px 参考字号约 **64px**，
    即 ~1.45em）时按"描边被切短"降级 + warn-once（距离场能表示的最远距离到顶了）。
  - **几何描边（真 miter / bevel / round 连接）**：`TextStroke` + `TextStyle::vector_outline` /
    `with_vector_stroke`。与距离场描边互补 —— 距离场描边天生圆角连接且改宽度免费，
    几何描边用 zeno 的 stroker 生成轮廓再栅格化，尖角精确（SVG 观感），复用覆盖度图集
    （`content_type = Mask`），**不需要新管线也不需要改着色器**；`miter_limit` 超过退化成 bevel。
  - **图层 = 距离场上的区间**：`TextLayer.cutoff`（上界）+ `TextLayer.above_fill`。内描边是
    `(−w, 0]` 的**环**、内阴影是字形内的带，都画在填充之上 —— 半空间做不到这两者
    （会被填充整个盖住）。新增 `TextLayer::band(..)`、`TextStyle::inner_shadow(..)`。
  - **合成顺序按层跨字形排序**：每个字形每层一个 quad，所有字形先铺完第 0 层、再第 1 层……
    最后填充；否则相邻字形后一个的同类层会吃掉前一个的描边/发光（紧排文字很明显）。
  - **内阴影用两个采样点**：`cov = 字形内部(未偏移) − 偏移后的覆盖率`；只用一个采样点做差
    在字形内部恒为 0，只剩边缘一条缝（看起来像发光）。
  - 层的偏移采样**夹紧在本字形的图集矩形内**，避免带偏移的层读到隔壁字形数据（边缘细框）。
  - **逐段样式**：`TextPart::xxx_def(_, def)` 里的 `def.style` 按段生效（与字号/字体同一套语义）。
  - **可观测**：`GpuContext::text_atlas_stats()` → `TextAtlasStats`（R8 / 彩色图集尺寸与字节数）；
    样式文本不进彩色图集（有不变量用例盯着）。
  - 实测（`tests/gpu_bench.rs`）：3px 描边使覆盖面积 404 → 967 px 且 556 px 为描边色；
    同色描边的 258 个深内部像素与无样式**逐像素一致**；软投影重心在文字右下；
    页面矩形渐变上红下蓝（R/B 1.32 vs 0.56）；纹理填充左红右蓝（89089/24005 vs 0/35868）；
    emoji 描边 3253 → 4595 px 且 2145 px 为描边色；几何描边 10px 让纵向跨度恰好 +10px；
    内描边是环（红环 2816 px + 环内白 1613 px、轮廓不外扩）；紧排 "HH" 白色面积比值 2.00。
  Text styles via a signed-distance-field layer stack: `TextDef.style` + `TextStyle` (outline,
  multi-outline, soft shadow, glow, inner outline, inner shadow, custom fill). Styled glyphs get an
  exact distance field derived from the font outline, sharing the existing R8 atlas; unstyled text
  is byte-for-byte unchanged. Soft edges use an error function (one tap for blurred shadows).

- **渐变通道（轴向 / 线段 / 折线，覆盖全部 SDF 形状）**：新增
  `draw_rect_gradient` / `draw_rounded_rect_gradient` / `draw_circle_gradient` /
  `draw_line_gradient` / `draw_shape_gradient`（+ `DrawBatch` 镜像），由 `GradientStop`
  （`(位置, 颜色)` 停点）与 `LinearGradient`（角度 + 停点）描述。
  - **渐变坐标由形状决定**：线段沿 `p1→p2`（`draw_line_gradient` 没有角度参数）、
    折线沿弧长（与 `draw_line_chain_colors` 共用同一通道）、其余形状沿 `angle_deg`
    并**跨满形状包围盒**（0° = 自上而下，90° = 自左而右）。
  - **一个形状 = 一个实例 = 一次 draw**，与单色绘制完全同一条路径：没有额外 batch、
    没有额外 bind group。渐变坐标在顶点着色器算（仿射 ⇒ 插值后逐像素精确），
    片元只多一次色带查表。
  - 色带**按内容去重**（同停点 + 同角度 ⇒ 一份数据，与尺寸无关），平台/淡出用停点位置
    表达（`(0,c),(0.62,c),(1,d)`），半透明压暗不再需要「切 N 条窄带」。
  - **预乘插值**：透明 → 深色不会再出现边缘发暗；停点可重复（硬边）、可越界（平台）、
    顺序无关。
  - 1 个停点或全等色**退化为单色绘制**，与既有路径**逐像素一致**；旧路径（无渐变）
    逐像素不变（实例的渐变索引恒为 0）。
  - **只走 SDF 路径**：渐变绘制会强制 SDF 实例（`sdf_feather` 只决定边缘软硬，
    `None` ⇒ 硬边），不做静默单色回落。geo 模板 / mesh 路径 / 文字 / 自定义材质不支持
    （文档写明）。
  - 实测（`tests/gpu_bench.rs`）：8 种形状「单色渐变 ≡ 单色绘制」逐像素一致；竖直渐变与
    理想 ramp 线性偏差 ≤ 4.3/255 且逐像素无台阶；圆角外像素为 0；线段渐变与两点折线渐变
    逐像素一致；贴图 × 渐变 = 两者相乘。新增 `docs/reference/gradient.md` 与
    `examples/gradient.rs`。
  Gradient channel for all SDF shapes: `draw_rect_gradient` / `draw_shape_gradient` /
  `draw_line_gradient` (no angle — the segment is its own axis), `GradientStop`-based stops,
  per-shape gradient coordinates, one instance per shape on the existing SDF path, content-deduped
  ramp table shared with the polyline gradient. Solid-only paths are pixel-identical when no
  gradient is used (the instance gradient index stays 0).

- **折线沿线渐变（逐点色）**：新增 `draw_line_chain_colors(batch, points, thickness, colors)`
  与 `DrawBatch::line_chain_colors(..)` —— 第 i 个点取 `colors[i]`，两点之间**沿弧长**
  平滑过渡，颜色在片元里逐像素求值。整条链仍然是 **1 个 SDF 实例 / 1 次 draw**
  （不是切成 N 段单色：那样每段的圆头会在接头处重叠并被 blend 两次，半透明时接头出亮点）。
  - 覆盖「框内保持、框外淡出」这类需求：alpha 按每个点的距离算进 `colors` 即可，
    不再需要「切成 4 档单色」那种肉眼可见台阶的做法。
  - 内部编码是**按实例寻址的弧长色带表**（新 storage binding `group(2)@binding(3)`，
    `array<vec4<f32>>`），因此**不与 `polygon_edges` 平行**：同一份几何（模板缓存命中）
    可以随时换色带，不存在"边对色错"；几何模板缓存与哈希完全不用动。
  - 逐点色先转成 `(弧长, 预乘色)` 折线，再经 **Douglas–Peucker 简化**（误差 ≤ 1/255）：
    200 点线性渐变只占 2 个 stop，"常量 + 淡出 + 常量"典型 4~6 个；超过 64 个 stop
    的极端输入会被均匀降采样。
  - 混合在**预乘空间**做（红 a=0.9 → 蓝 a=0 的中点必须是无偏的红，而不是偏紫），
    再反预乘输出给 straight-alpha 的 blend state。
  - **旧路径零影响**：单色链的 `sdf_params.w == 0`，片元走逐字保留的原分支；
    实测「全同色逐点色」与 `draw_line_chain` **逐像素一致**。
  - 降级（不 panic）：色数 ≠ 点数 → 按 `colors[0]` 单色 + warn once；
    几何路径（`sdf_feather = None`）→ 单色 + warn once；全透明 → 跳过；重合点 → 跳过该段；
    自定义材质不生效（色带由引擎片元读取）。
  - 实测（`tests/gpu_bench.rs`）：200 点 alpha 0.8→0.12 与线性 ramp 最大偏差
    **0.62/255**、无台阶；同点集不同色带互不串色；PerSample(4) 档正常。
  新增 `docs/reference/line-gradient.md` 与 `examples/line_gradient.rs`。
  Per-point colored polyline: `draw_line_chain_colors` interpolates color along arc length
  within a single SDF instance (no per-segment splitting, no double-blend at joints), via a
  per-instance arc-length ramp table decoded in the fragment shader. Old single-color chains
  are pixel-identical (the new branch only runs when `sdf_params.w != 0`).

- **关窗拦截 + 关窗/退出收尾钩子（四个，全部新增）**：此前只有
  `App::window(desc, on_close)` 那个建窗时传入的 `FnOnce` 事后通知 —— 没有返回值、
  拦不住关闭、不能运行期注册；而 main 返回 / 设备丢失 / main panic 这些收尾路径上
  没有任何用户回调。现在：
  - **`on_close_requested(handle, FnMut(CloseSource) -> CloseAction)`**（`App` 与
    `VireoWindow` 都有）：返回 `CloseAction::Cancel` 即**取消**本次关闭，窗口原样存活
    （不隐藏、不销毁、`on_closing` 不消耗）。参数 `CloseSource::{User, Programmatic}`
    区分「点 X」与「`win.close()`」，于是「X 要问、程序化退出直接走」可以直接表达。
    用枚举而非 `bool`：极性写反的代价是两极的（永远关不掉 / 静默丢数据）。
    可多次注册（全部 `Allow` 才关，遇 `Cancel` 短路）。覆盖 X / Alt+F4 / 自绘标题栏
    关闭按钮 / `win.close()`；跑在 **winit 线程且同步**（与所有 `on_*` 一致）⇒ 不许阻塞、
    不许画帧，确认框要「置旗 + 返回 `Cancel`，让 loop 下一帧画」。**刻意不问**的路径：
    main 返回后的批量收尾、设备丢失、Android Destroy、OS 强行销毁 —— 那些路径上没人能
    驱动 UI，问了只会把进程锁死。单条 panic 按「放行」处理（fail-open）+ `log::error!`。
  - **`on_closing(handle, FnOnce())`**：这个窗口**已获准关闭**、即将拆除时触发一次，
    跑在 winit 线程。多窗口程序里唯一能区分「关的是哪一个」的地方。只覆盖**主动关窗**
    （X / `close()`）—— main 返回后的批量收尾与设备丢失不走 winit 关窗路径，专用
    `on_exit` 收尾。
  - **`on_exit(FnOnce(ExitReason))`**：进程退出前**恰好一次**，跑在 winit 线程，
    顺序是「钩子全部跑完 → 回 ack → `event_loop.exit()`」。新增
    `ExitReason { MainReturned, MainPanicked, AllWindowsClosed, DeviceLost }`
    （优先级 `DeviceLost > MainPanicked > MainReturned > AllWindowsClosed`）。
    单条 panic 被隔离，不影响后续钩子与退出流程。
  - **`on_exit_render(Fn())`**：**每个 loop 线程各一次**，在该线程上跑，能碰
    `App` / `VireoWindow` / GPU（提交 compute、读回 buffer、drop 纹理）。
    时机在 `state.done` 之前，所以 main 返回型退出里「渲染侧收尾先于 `on_exit`」有保证；
    末窗关闭 / 设备丢失两条无顺序保证。**不保证还能 present**（surface 多半已被 drop）。
  四个钩子全部走统一注册表（`App::on_*(handle, cb)` / `VireoWindow::on_*(cb)`），
  未注册时都是空表早退，退出行为与 0.1.x 逐位一致。新增
  `docs/reference/lifecycle.md`（含四条路径的覆盖表）与 `examples/exit_guard.rs`
  （四档手工验收 + `VIREO_EXIT_GUARD_AUTO=1` 自检模式，实测「否决后窗口存活 → 放行后
  正常退出」）。
  Close-request veto + close/exit hooks: `on_close_requested` can cancel a close
  (`CloseSource` in, `CloseAction` out; X and `close()` share one path; winit-thread sync;
  fail-open on panic), `on_closing` fires per window once the close is approved,
  `on_exit` runs once per process (winit thread) with an `ExitReason`, and
  `on_exit_render` runs once per loop thread on the render thread with GPU access.
  All four are registered the same way and can be added at runtime.

- **GPU 时间对所有绘制可见（不再只是粒子）**：相机 uniform 是 80B
  （projection 64 / dpi 4 / `time_secs` 4 / pad），引擎每帧把默认钟时间写进 offset 68，
  所以形状、文字、粒子、custom VS 与 custom material 都能读同一份 GPU 时间：
  WGSL 里声明 `struct Camera { projection: mat4x4<f32>, dpi_scale: f32, time_secs: f32 }`
  即可（`@group(0) @binding(0)`），CPU 每帧不用改任何 uniform。
  `DrawBatch::clock`（默认 `ClockIndex::ZERO`）可把某个 batch 换到另一条时间线：
  非默认钟会在 draw 时绑定该时钟自己的 80B camera buffer（投影/dpi 相同、时间不同），
  默认钟仍走共享组、零额外绑定。配合 `set_clock_scale(0.0)`（暂停）与
  `set_clock_time(t)`（定值/确定性回放）使用。
  GPU time is now visible to every draw path, not just particles: the camera uniform
  carries `time_secs`, refreshed per frame for the default clock; `DrawBatch::clock`
  switches a batch to another timeline (paused/scaled/set-value supported).

- **按需出帧（脏驱动）示例**：`examples/on_demand.rs` 演示"这一帧不画"这个能力——
  只在输入到达（`refresh_input`）、尺寸/DPI 变化（`resize_pending`）、自身状态变化
  （含主动持续动画）时调用 `win.draw`。空闲期间 0 次 present、不 acquire、不 encode、
  屏上保留上一张已 presented 的图（实测：空闲 0 present/s，CPU ≈ 0.5% 单核，debug 构建）。
  示例同时展示"主动在一段时间内持续更新"（0.9s 脉冲、`A` 键持续动画）与"按需请求一帧"
  （`R` 键），并用窗口标题做不需要出帧的实时反馈通道。
  On-demand (dirty-driven) rendering example: skip `draw` entirely when nothing changed —
  no acquire/encode/present; plus opt-in continuous animation while the app wants it.
- **空闲自动降频（默认开启，30fps）**：画面**没上屏**时（DWM 判定遮挡 / 最小化 / 0×0 /
  surface stale / acquire 超时）把帧率压到 `idle_cap`（默认 `Some(DEFAULT_IDLE_FPS)` = 30，
  与用户 `max_fps` 取更严者），避免后台挂着时全速空转。
  触发依据是**可见性事实**，不看焦点、也不看 vsync 是否在钳制——所以主动关掉 vsync 的前台
  窗口不会被误降频。关闭：`WindowDesc::idle_cap(None)` 或 `win.set_idle_cap(None)`。
  新增 `DEFAULT_IDLE_FPS` 常量、`WindowDesc::idle_cap` / `VireoWindow::set_idle_cap` /
  `idle_cap()` / **`applied_cap()`**（当前真正生效的上限，用来区分"被谁压着"）。
  Idle auto-throttle to 30fps when the window is not on screen (occluded/minimized/etc.),
  independent of focus and of vsync throttling; opt out with `idle_cap(None)`.
  `applied_cap()` reports which cap is actually in effect.
- **`VireoWindow::is_vsync_throttled()`：实测「present 有没有被 vblank 钳制」**。
  比较 `DrawTimings.acquire_secs + present_secs` 与显示器刷新率（`1/hz`，取不到回退
  60Hz 假设），连续 8 帧同向证据才切换（滞回，避免抖动）；建窗与 `Resized` 时刷新
  刷新率缓存。配合 `DrawReport::vsync_throttled`（同一份实测值）使用。
  典型用途：`max_fps = None` 时判断「vsync 是否已经不兜底了」——Windows 上 DWM 判定
  窗口被遮挡后 DXGI 会提前返回，此时循环会全速空转（有数百毫秒到一两秒的延迟）。
  Measured vsync throttling state on `VireoWindow` (acquire+present block time vs
  refresh period, 8-frame hysteresis), replacing the old focus-based guess.

- **文件读写 API**：安卓的「文件」有三个互不相通的世界，此前 vireo 一条都没覆盖
  ——随包资源在 APK 内部（`std::fs` 读不到），且没有任何可写目录概念。
  - **读（随包资源，只读）**：`App::assets() -> Assets`，含
    `read` / `exists` / `list`；配套 `App::load_texture_asset` /
    `App::load_font_asset` / `WindowDesc::icon_from_asset`。
    安卓读 APK 内 `src/main/assets/`（经 `AssetManager`），桌面读
    `App::set_asset_root` 指定的目录（默认流程 `./assets`）—— **同一份代码两端通用**。
  - **写（可写目录）**：`App::data_dir()`（持久私有）/
    `App::cache_dir()`（可再生）/ `App::external_data_dir()`（用户可见，可为 `None`）。
    安卓 = `internal_data_path` / `Activity.getCacheDir()` / `external_data_path`，
    全部免权限；桌面按平台标准目录（`%APPDATA%` / `%LOCALAPPDATA%\Cache` /
    `$XDG_DATA_HOME` / `$XDG_CACHE_HOME` / `$XDG_DOCUMENTS`）。
    末级名字默认取可执行文件名，`App::set_storage_name` 可改。
  - **刻意不做**「相对路径自动回退到 assets」。那会让同一份代码在两个平台上
    **资源来源不同且不报错** —— 桌面调通了、安卓静默换一张图，排查成本极高。
    `load_texture` / `load_font_file` / `icon_from_path` 语义**完全不变**（纯 `std::fs`）。
  - 资产名归一化接受 `\` 与前导 `/`（同一字符串两端都能用），**拒绝 `..`**
    （它会参与路径拼接，能跳出根目录）。
  - 新增错误变体 `VireoError::AssetNotFound` / `NoAssetRoot`，
    区分「资源不存在」与「桌面没设根目录」。
  - 读取走 `Asset::read`，压缩与未压缩资产都能读，**不依赖** `noCompress` 配置。
  - 示例 `examples/resource_io`；assets 脚手架 `examples/assets/` 与
    `examples/android/src/main/assets/`。

- **触摸状态轮询 API**：`VireoWindow::touches() -> Vec<TouchPoint>` /
  `touch_count()` / `touch_first()`，以及公开类型 `TouchPoint`
  （`{ id, pos: PixelPos, force }`）。此前触摸没有类型安全的读法 ——
  `InputState.touches` 字段虽为 `pub`，但类型是裸
  `HashMap<u64, (f32, f32, Option<f64>)>`（无排序保证、无文档），实际只能靠
  `on_touch` 回调，鼠标式命中判定做不了。
  - `touches()` 按 `id` 升序（底层 `HashMap` 迭代顺序随机，不排序会让「取第一个点」
    每帧拿到不同手指）；`touch_first()` 与 `touches().first()` 逐位一致。
  - `pos` 用 `PixelPos` 双视图，与 `mouse_pos()` 同构；由逻辑值现算 `.px`
    （`dpi::dp_of`，**不是** `to_pixel_pos` —— 方向相反，用错会再除一次 scale）。
  - ⚠️ 轮询是**电平**语义：手指按住期间每帧都在结果里。「按一下触发一次」必须
    自己按 `id` 差分，或改用 `on_touch` 的 `Started` 边沿。文档内有示例。
  - 触摸**仍不**模拟鼠标按键：`mouse_left()` 在纯触摸设备上恒为 `false`。
- Android `onStop` 生命周期：`App::on_stopped` 回调 + `App::is_stopped()` /
  `android_lifecycle::is_stopped()`。只对「暂停期间 activity 变得不可见」的
  暂停触发，用来区分「半透明弹窗 / 权限对话框」这类短暂中断与「回桌面 / 锁屏」
  这类彻底离开（`onPause` 两者都触发）。
  `is_stopped()` 是「已暂停 && 本次暂停发生过 onStop」的派生量，恢复后自动翻回。
  Android `onStop` lifecycle: distinguish a brief interruption from a real departure.
- **生命周期暂停闸门可接管**：`App::set_auto_pause(false)`（默认 `true`，老代码零改动）。
  关掉后切后台不再阻塞渲染循环，由用户自己决定跑什么 / 画不画 —— 引擎不替用户
  决定功耗策略。「策略」（`auto_pause()`）与「状态」（`is_paused()`）刻意用不同词。
- **「暂停直到」交给用户**：`App::wait_while_paused()`（等前台，可打断）、
  `App::wait_until(timeout, cond)` + `App::notify_waiters()`（通用条件）。
  会被窗口关闭 / 设备丢失 / activity 销毁打断，不会挂死；平台生命周期变化自动唤醒，
  自己的条件变化需自己 `notify_waiters`。
- Android `onDestroy` 生命周期：`App::is_destroyed()` + `App::on_destroyed()`。
  唯一能回答「用户是切走了还是主动关了我」的信号（`is_paused` / `is_stopped`
  在划掉任务时都会照常触发），也是 `set_auto_pause(false)` 之后唯一的退出条件。
  ⚠️ **进程被系统杀掉时不触发**，所以不能用于资源清理或存档。
  Lifecycle gate is now user-controllable; wait primitives and `onDestroy` exposed.

- GPU 粒子路径：`Particle` / `ParticlePool` / `ParticleSlot` / `draw_particles`
  （`spawn` 参数烘焙＋VS 按 `camera.time` 积分位移与 fade 包络，存活期 CPU 零更新）。
  GPU particle path: baked spawn params + VS integration over `camera.time`.
- 粒子时钟注册表：`ClockIndex`（generational）＋`create_clock` / `destroy_clock` /
  `set_clock_scale`（0＝暂停，恢复无跳变）/ `set_clock_time` / `clock_time`；
  多独立时间线，0 号默认钟。
  Particle clock registry: multiple independent timelines, default clock 0.
- `DrawBatch::particle_clock`（batch 属性，默认 0 号钟）＋Renderer per-clock camera 组：
  同 batch 同钟，多批多钟同屏各走各的时间线（单 pass，零管线变动）。
  Per-batch particle clock + per-clock camera groups: independent timelines on screen.
- `ShapeStats::particles` 诊断字段（`shape_vertex_count` 含粒子等效顶点）。
- `Texture` address mode：`set_address_mode`（原地切换）/ `with_address_mode`
  （共享 image 的新对象，同帧多 mode 混用）＋prelude `AddressMode`
  （直接用 `wgpu::AddressMode`；默认 ClampToEdge，老行为不变）。
  Texture address modes: switch in place or share one image across modes.
- 混合全透传：`DrawBatch::set_blend_state` / `clear_blend_state` /
  `set_blend_constant`（形状＋文字，逐段冻结；语义 = wgpu 原样，用户自负；
  `Src1` 要 dual-source feature，vireo 不开）＋prelude `BlendState` 等。
  Raw blend-state passthrough: per-segment freeze for shapes and text.
- `ParticlePool::sweep`（单遍清除过期粒子，无分配；示例每 30 帧调用一次）。
  `ParticlePool::sweep` (single-pass expiry without allocation).
- 粒子 G1~G5：`rotation`（只转网格）＋`damping`/`gravity`（原始指数语义闭式积分，
  正=增长、衰减填负；`q=d*t` 分段 Taylor 保 f32 稳定）＋`size_mul_end`/`color_end`
  （alpha 仍只走包络，`end.a` 忽略）＋`AtlasSeq` 图集序列（行优先）＋
  `set_max_live`/`OverflowPolicy::{DropNewest, KillOldest}`（堆维护，摊销 O(log n)；
  cap 为 0 全拒；降 cap 不追溯）＋prelude `AtlasSeq`/`OverflowPolicy`。
  Particle G1-G5: rotation, damping/gravity closed-form, size/color endpoints,
  atlas sequences, pool cap with heap KillOldest.
- 粒子 `fade_mid` 包络中点（`Option<f32>`，默认 None = smoothstep；设值后 fade_out 段
  改线性三点 `1.0 → mid → 0.0`，与时长正交，`clamp 0..1`；`fade_in` 恒 smoothstep）。
  Particle fade-out midpoint shaping (linear 3-point vs smoothstep).
- Android 平台支持（0.2.0 起全新提供；此前版本无任何安卓适配）：
  - 入口与构建：`#[vireo::main]` 生成 GameActivity 胶水（`InputConnection` 正常，
    中文输入可用）＋`cargo ndk` / gradle 模板（`examples/android`，
    `xbuild` / `cargo-apk` 不再适用）＋ targetSdk 35（挖孔全宽）＋ 16KB 页合规。
  - 窗口与系统：`WindowExtAndroid`（系统栏 / 安全区三值 / 挖孔 / 朝向 / 常亮 /
    亮度 / 层级）＋真实刷新率＋深浅色读与通知（复用 `theme()`，不新增 API）＋
    `configChanges`（字号 / 语言不重建）。
  - 输入：`TextInputState`（UTF-16/UTF-8 下标换算）＋返回键接管
    （`App::on_back` / `consume_back`）＋剪贴板（文本 / HTML，免费函数）＋
    振动（once / predefined / pattern / cancel，免费函数）。
  - 文件：SAF（tree / open / create＋`SafUri` 持久化跨启动复用）——资产与可写目录
    见上「文件读写 API」条。
  - 生命周期（安卓侧）：resume surface 重建＋返回键退出防僵尸（跨平台闸门见上相关条）。
  - 文字与弱 GPU：系统字体白名单加载（可定制扫描）＋ COLRv1 emoji＋
    noflat GLES 回退＋离屏格式漂移自愈。

- **GPU Compute 入口（P5）**：`App::compute_pipeline(wgsl, entry, &[(group, &bgl)])` /
  `GpuContext::create_compute_pipeline` 创建 compute 管线（用户自带 BGL 与任意 group 编号，
  复用 P3 的自管 group 模型；WGSL/布局错误返回 `Err` 而不是 panic）；
  `App::dispatch(&kernel, &[(group, &bind_group)], [x, y, z])` / `GpuContext::dispatch`
  提交一次 dispatch（独立 command buffer，提交边界自动 barrier —— 写入对后续 render pass
  可见）。新增 `App::gpu()` 暴露共享 `GpuContext` 以便自建纹理 / buffer / BGL。
  示例 `compute`（compute shader 每帧写粒子场到 storage texture 再贴回窗口）。
- `App::on_memory_warning(cb)` —— 跨平台内存压力回调（winit `Event::MemoryWarning`，
  Android 来自 `onTrimMemory`、iOS 来自 memory warning notification）。在**渲染线程**
  上触发，单个回调 panic 不打断渲染循环。**零 JNI**：winit 是唯一把 Android 生命周期
  事件正常转发出来的那个（`MainEvent::LowMemory`，`platform_impl/android/mod.rs:265-267`），
  其余 `Stop` / `Destroy` / `SaveState` 都是 `warn!("TODO")`。
  拿不到 Android 的具体 level（AGSK 的 C glue 只 `LOGV` 打印后写无 payload 的
  `APP_CMD_LOW_MEMORY`），但响应动作在所有 level 上相同。
  ⚠️ 这**不是「vireo 帮你防 OOM」**：vireo 侧能回收的只有 glyph atlas，
  显存大头（用户贴图）因 `App.textures` 是 append-only 而**结构上释放不了**。
- `DrawBatch::clip_self`（默认 `false`）：让本 batch 自身内容也受自己的矩形
  `scissor` 裁剪。默认仍只裁子 batch（旧行为不变）；`clips_children` /
  `scissor` / `text_clip` 的作用域与坐标空间已在字段文档写清（不再依赖行内注释）。
  `DrawBatch::clip_self`: the batch's own content now opts into its scissor window.
- `Area::rect(Rect)`：由逻辑像素矩形直接构建 Area（SDF 掩码），免去手搓
  `AreaGeom`；`Area::geom` 在 debug 构建校验索引（整三角形、不越界），
  SDF 路径却没有 `sdf_type != 0` 顶点时首次 `warn!`（掩码会退化成外接矩形）。
  `Area::rect`: build a rectangular stencil mask from a logical rect.
- `DrawBatch::clip_path() -> ClipPath`：查询本 batch 实际走的裁剪路径
  （`None` / `AreaStencil` / `Scissor { rect, automatic, clips_self }` / `Stencil`），
  纯查询、不推进状态。矩形裁剪请优先 `scissor`（见 `area` 模块文档）。
  配套 `Renderer::last_clip_paths()` / `VireoWindow::last_clip_paths()` /
  `OffscreenCanvas::last_clip_paths()`：上一帧实际参与绘制的 batch 裁剪路径表。
- `DrawBatch::merge_instance_states`（默认 `false`）：`preserve_order = false` 时把
  SDF 实例按 `(bind group, material, blend, blend constant)` 重排到连续范围，使同状态
  实例可合并 draw call（1000 个交替 blend 实例从 1000 dc → 2 dc）。代价同 `preserve_order`
  ——同 batch 内层叠顺序不再保留。
  `merge_instance_states`: reorder instances by draw state to merge draw calls.
- `VireoWindow::draw_with_passes` / `OffscreenCanvas::draw_with_passes`：在引擎 pass
  **之后**用同一个 `CommandEncoder` 追加自定义 pass（后处理链、额外 compute），
  整帧只 submit 一次；引擎 pass 的 DS 不延续，需要时由回调自建。
  `draw_with_passes`: append custom passes to the same encoder after the engine pass.
- `PreparedFrame` + `draw_with_inline` / `draw_with_frame`：把“准备”与“编码”拆开，
  两种用法共用一条流水线。
  - `draw_with_inline(|pass| ..)`：回调在**引擎 pass 内部**、引擎批次之后执行，
    pass 的颜色/MSAA resolve/DS 已配对好（同 pass 叠加 UI/描边，无额外全屏采样）。
  - `draw_with_frame(|frame, encoder| ..)`：回调自建 pass，用 `frame.encode(&mut pass)`
    写入引擎批次。`PreparedFrame` 暴露 `target()` / `target_format()` /
    `sample_count()` / `has_ds()` / `is_empty()`。
  - **契约**：自建 pass 必须复用 `frame.target()` 的同一组颜色 view（数量/格式一致）；
    `sample_count() > 1` 时需自建 MSAA target 并 resolve；`has_ds()` 为真时必须挂
    `Depth24PlusStencil8`；尺寸须与 renderer 一致。**不支持只编码部分批次**，
    多次 `encode` 会重复绘制全帧。
  - 嵌套 `prepare`（回调里再次 `draw*`）会 panic 并给出明确消息，不会静默死锁。
  `PreparedFrame` + `draw_with_inline` / `draw_with_frame`: prepare/encode split with
  an engine-pass hook and a user-created pass.
- **采样契约类型**：`TargetAa { samples, per_sample, alpha_to_coverage }`（`none` /
  `msaa` / `per_sample` / `with_alpha_to_coverage` 构造 + `From<AntiAliasing>`）；
  `RenderTarget::aa`（公开字段）/ `with_aa` / `set_aa`；`PreparedFrame::target_aa()`；
  `VireoWindow::anti_aliasing()`。自建 pass 时可据此逐项对齐附件。
- **顶点布局 ABI 公开**：`ShapeVertexLayout`（`Mesh` / `SdfInstance` / `GeoInstance` /
  `Particle`，prelude 导出）与 `Material::shape_layout()`，让自定义顶点着色器能在
  创建期显式声明契约、而不是靠编译结果猜。

### Changed / 变更

- **BREAKING：`App::window` 去掉 `on_close` 第二参数**（`app.window(desc)`）。
  那个参数只能在建窗时传、且职责与新增的 `on_closing` 完全重合，已删除；
  等价迁移是建窗后 `app.on_closing(idx, || ...)`（运行期建窗也能挂）。
  调用点删掉 `, None::<fn()>` 即可 —— 示例里那个处处可见的 turbofish 一起消失。
  `App::window` no longer takes an `on_close` callback; use `app.on_closing(idx, ..)`.

- **BREAKING｜`AntiAliasing::Ssaa` 改名为 `PerSample`（原名误导）**：这一档实际做的是
  **逐采样点着色**（`@interpolate(linear, sample)`，每个采样点独立求值后 resolve 平均），
  规范术语是 sample shading（Vulkan `sampleShadingEnable`）。叫 SSAA / SGSSAA 会让人以为
  它像 2× 渲染再缩采样那样**全要素**超采样——实际只超采样 **SDF 几何覆盖度**：
  纹理采样用的 `uv` 仍是逐像素插值（N 个采样点同值），屏空间后处理（blur/bloom）也不受益。
  迁移：`AntiAliasing::Ssaa { .. }` → `AntiAliasing::PerSample { .. }`、
  `AntiAliasing::is_ssaa()` → `is_per_sample()`、`TargetAa::ssaa(n)` → `TargetAa::per_sample(n)`。
  `PerSample` 仍是 SDF 形状唯一有效的硬件 AA 档（形状画成整像素四边形，MSAA 的采样点
  覆盖率对它无效）；自定义 shape VS 在该档仍按像素求值。该档**保留**解析软边（不把
  `feather` 归零交给 resolve）：45° 边实测「软边 + 逐采样」过渡像素 204 / 覆盖率级数 50 /
  单像素最大阶跃 209，「硬边 + resolve」只有 92 / 3 / 255 —— 4 个采样点下软边更平滑。
  `AntiAliasing::Ssaa` is renamed to `PerSample`: it is per-sample shading (the standard term),
  not supersampling of the whole frame — only SDF coverage is supersampled, `uv` stays
  per-pixel and screen-space effects are untouched.

- **BREAKING｜`DrawBatch::particle_clock` 改名为 `clock`**：时间线现在对**所有绘制**
  生效（形状/文字/粒子/custom VS 与 custom material 都从 group 0 的
  `camera.time_secs` 读），不再是粒子专属属性。新增 `set_clock` / `clock()`；
  `set_particle_clock` 保留为 `#[deprecated]` 转发。
  Migration: `batch.particle_clock = c` → `batch.set_clock(c)`; field is now `clock`.
- **`wgsl_snippets()` 从 prelude 移除**：`// #include "vireo_*.wgsl"` 机制不变
  （`material::expand_includes` 仍在 prelude 外可用），只是不再把 3 段工具函数的映射表
  挂在 prelude 上；需要时 `vireo::material::wgsl_snippets()`。

- **文档｜`docs/reference/throttling.md` §4.4 改错并重写为「按需出帧」**：原文拿
  `on_tick` 返回 `false` 当"本帧不画"，而 `false` 实际表示该 loop 结束（并连带停掉整条
  渲染线程）。改成真实可行的形态——不调 `win.draw` + 四个脏判据（`refresh_input` /
  `resize_pending` / `refresh_metrics` / 自己的状态），补"跳过帧时仍要 `refresh_input`"、
  "动画收尾补画一帧"两个坑，并记入 `presented_fps()` 空闲不衰减（第 5 条）与"按需出帧
  不改变 tick 频率"（第 6 条）两条已知限制。
  Fixed a wrong recipe in the throttling guide (`on_tick` returning `false` ends the loop —
  it does not skip a frame) and rewrote it as the supported dirty-driven pattern.

- **BREAKING｜移除 `App` 的全局帧率旋钮**：`App::set_max_fps` / `max_fps` /
  `set_drag_cap` / `drag_cap` 全部删除。它们从来不是运行期全局控制——只在**建窗/spawn
  那一刻**给窗口和线程种默认值，名字却读成"立刻改全局帧率"。
  改为**声明式 + 组件自带默认**：`WindowDesc::max_fps(impl Into<Option<u32>>)`（默认
  `Some(DEFAULT_MAX_FPS)` = 240）、`WindowDesc::drag_cap(bool)`（默认 `true`）；
  新增公开常量 `DEFAULT_MAX_FPS`。运行期仍用 `VireoWindow::set_max_fps` /
  `set_drag_cap`（窗口各自持有，无全局值）。
  迁移：`app.set_max_fps(Some(60))` → `WindowDesc::… .max_fps(Some(60))`。
  Removed the illusion of an app-wide frame cap; per-window defaults are now declared
  on `WindowDesc` and live on the window.
- **BREAKING｜`Thread::max_tps = None` 现在真的表示「不限速」**：此前 `App::spawn` 里
  `thread.max_tps.or(app.max_fps())` 会把 `None` 替换成 App 的值（而 `None` 又是字段
  默认值，于是"没设过"和"想不限制"无法区分）。现默认值改为
  `Some(DEFAULT_MAX_FPS)`，不再回落到 App。
  `Thread::max_tps` is now `Some(240)` by default and `None` truly means uncapped.
- **BREAKING｜`RenderAdvice` 只回答「该不该画」，`Unthrottled` 变体移除**：
  `RenderAdvice` 现在只有 `Render` / `Skip`，`render_advice()` 不再用「是否聚焦」猜测
  节流状态（失焦但可见的窗口仍受 vsync 约束，旧判定会误报）。原来 `Unthrottled` 想表达
  的「没被 vblank 钳制」改为实测事实，见新增的 `VireoWindow::is_vsync_throttled()`；
  `DrawReport::vsync_throttled` 也从「有焦点」改为实测值。
  `RenderAdvice` now only answers "should this frame be built" (`Render`/`Skip`); the
  throttling question moved to the measured `is_vsync_throttled()`.

- **BREAKING｜采样契约随目标传入**：新增 `TargetAa { samples, per_sample,
  alpha_to_coverage }` 与 `RenderTarget::aa`（`with_aa` / `set_aa`，构造期从附件推导）。
  `Renderer::new` 去掉 `aa` 参数，移除 `Renderer::update_aa`；`AntiAliasing` 退化为
  窗口/离屏自己的配置（`VireoWindow::anti_aliasing()` 可读）。AA 的 5 处同步点
  （update_aa / resize 清理 / msaa_view 比对 / ds_view 比对 / pending_aa）随之消失：
  引擎 MSAA/DS 视图按 `samples` 逐附件惰性创建，运行期 `set_anti_aliasing` 不再抢
  渲染器锁。附带能力：**MRT 与 MSAA / per-sample 可同时使用**（此前引擎 MSAA 只服务单目标）。
  Sampling is now a property of the render target (`TargetAa`) instead of renderer
  state; MSAA/depth views are allocated per attachment, so MRT + MSAA works.
- **BREAKING｜custom VS 必须显式声明顶点布局**：`ShapeVertexLayout` 公开并进入 prelude
  （`Mesh` / `SdfInstance` / `GeoInstance` / `Particle`）；
  `App::material_with_vertex_shader`、`material_with_resources_and_vertex_shader`、
  `material_manual_with_vertex_shader` 及 `GpuContext` 同名入口新增必填 `layout`。
  创建期只验证声明的那一个布局（失败即报错），不再用"哪个布局能编译通过"反推支持集
  （那会放过"类型匹配、编译通过、语义错"的着色器）；声明之外的绘制路径跳过并 warn-once，
  不回落 mesh。`Material::shape_layout()` 可查询。
- **BREAKING｜贴图归调用方所有**：`App::load_texture` / `load_texture_asset` 返回
  `Arc<Texture>`，移除 `App::texture` / `AppInner.textures` /
  `VireoError::TextureNotFound`。引擎不再持有 append-only 的贴图表，最后一个引用 drop
  即释放显存（此前结构上无法释放）。
- **BREAKING｜移除 `CachePolicy`**：`AlwaysRebuild` 的"每帧重建"承诺被内部 bind group
  池按指纹命中掩盖，从未真正生效；`Dirty`（任一 `set_*` 置位即重建）是唯一实际语义。
  需要每帧重建的场合用 `set_bind_group_provider`。
- **MRT 不再按"附件数 > 1"关掉引擎能力**：删掉跨阶段传递的 `mrt` 布尔，判定落在使用点。
  引擎形状/文字现已能进多附件 pass（管线按附件数取变体，附加目标空写、保留载入内容）。
- 多边形/折线的边索引基址补丁（`sdf_params[0] += poly_base`）从 4 处重复实现收敛为
  `patch_instance_poly_base` / `patch_vertex_poly_base`；默认顶点着色器变体从四个
  `default_*_vertex_wgsl(ssaa, noflat)` 收敛为 `shape_vertex_wgsl(layout, ShapeShaderFlags)`。

- **渲染能力三项落地（P1 / P3 / P4）**：
  - **深度/模板细粒度控制**：新增 `DepthStencilAttachment` / `DepthStencilOps` /
    `DepthStencilTarget`（引擎 DS 或用户自带 view）；`RenderTarget::set_depth_stencil`
    （链式 `with_depth_stencil`）；`Material::set_depth_write` / `set_depth_compare`；
    `VireoWindow::draw_with_depth_stencil` / `OffscreenCanvas::draw_with_depth_stencil`。
    缺失 aspect 自动补齐并 warn-once。支持深度预通道＋着色通道跨 draw 保留深度。
  - **MRT 多目标**：`ColorTargetDesc { view, resolve_target, blend }` +
    `RenderTarget::from_color_attachments`；`VireoWindow::draw_mrt` /
    `OffscreenCanvas::draw_mrt`；`Renderer::draw` / `RenderTarget::draw` 改收
    `clear_colors: &[Option<Color>]`（与颜色附件等长）。custom material 返回
    `MaterialOutput`（第 0 个字段须名为 `color`，引擎在其上应用 alpha/SDF）即可写多目标；
    pipeline key 含目标数量与逐目标 blend，双目标不同 blend 一次提交。引擎形状/文字也能
    进 MRT pass：管线按附件数取变体，附加目标以"同格式 + 空写掩码"占位，只写 target 0、
    其余附件保留 `LoadOp::Load` 载入内容（此前被跳过并 warn-once）。反过来 MRT 材质也可
    用于单附件 pass（多余 fragment 输出被丢弃）。引擎 MSAA 自本次起按附件逐目标补齐
    （见下方采样契约条目）。**`draw_with_frame_mrt`**（窗口 + 离屏）把 `PreparedFrame`
    绑到多附件目标，自定义 pass 因此可与 MRT 组合。示例 `mrt`。
    Engine shapes/text now render inside MRT passes (color only; extra targets keep their
    loaded contents), and MRT materials work in single-target passes.
    `draw_with_frame_mrt` binds a `PreparedFrame` to multiple color attachments.
  - **Bind Group 去硬编码**：`App::material_self_managed(source, &[(group, &bgl)])` /
    `GpuContext::create_material_self_managed`——用户自带 BGL 与 group 编号，任意
    `@group(n)` 布局；自管材质不再绑定引擎 group 0/1/2。默认材质仍在 group 3
    （`MaterialState::B` 的 `group_index` 构造期固定）。示例 `custom_material_self_managed`。
- `OffscreenCanvas.texture` 改为 `Mutex<Texture>`（`draw` 内按当前
    `surface_format` 自愈重建，见下）；`view()` / `bind_group()` 改返回
    owned 克隆（原返回内部引用，与锁共存不了）。`Texture` 新增 `format` 字段。
    受影响的只有三处示例（`offscreen` / `offscreen_aa` /
    `custom_material_post`，`&canvas.texture` 改为持锁守卫）。
- SDF 实例路径改为 storage vertex-pulling：实例记录集中到 `@group(2) @binding(2)`
  storage buffer（`InstanceData`，112B，为 custom VS 提供可追加的稳定 ABI），
  内置 VS 用 `@builtin(instance_index)` 读取。公开类型 `ShapeInstance` 移除
  （`DrawBatch` 内部字段随之改名 `instance_data`）；渲染行为与 draw call 合并不变。
  SDF instancing now pulls `InstanceData` from a storage buffer (stable,
  append-only ABI for custom vertex shaders).
- **破坏性**：自定义顶点着色器（custom VS）不再回退 mesh，改为按绘制路径的契约参与实例化
  —— `sdf_feather = Some(..)` 走 SDF storage 契约（顶点输入仅 `QuadVertex` @0，
  实例经 `@builtin(instance_index)` 读 `InstanceData`）；`sdf_feather = None` 走 geo
  模板契约（`TemplateVertex` @0/@1 + `GeoInstanceInput` @2/@3）。创建期按
  Mesh / SdfInstance / GeoInstance 逐布局验证，全败才报错；形状所需布局不被支持时
  该形状被跳过 + 警告日志（不静默画错）。插值限定须用普通 `@interpolate(linear)`
  （`sample` 限定在 sample_count=1 验证失败）。示例与 GPU 回归已更新。
  Breaking: custom vertex shaders now plug into the instance paths instead of
  falling back to mesh; the shader must match the contract of its draw path
  (SDF storage contract or geo template contract).
- `preserve_order = false` 的段排序键纳入 material / blend / blend constant /
  geo 模板身份：同类段按状态聚拢，减少管线与绑定切换（**不改变**可合并条件——
  仍要求状态相等且底层范围连续，因此 draw call 数不变）。
  Reordered segments now sort by full draw state, reducing pipeline/bind switches.

### Fixed / 修复

- **dynamic uniform 槽换 buffer 后绘制仍读旧数据**：bind group 缓存用「资源句柄字段的
  地址」做指纹，而 `set_uniform_bytes` 对 dynamic 槽扩容时是**原地替换** buffer 句柄
  （地址不变）→ 指纹不变 → 命中旧 bind group，写入进新 buffer、绘制读旧 buffer；
  同一缓存还会无界增长（`clear()` 从未被调用）。改为按 `dirty` 重建后，`BindGroupPool`
  整体移除（每材质自身的 bind group 缓存 + `dirty` 已覆盖全部路径）。
  Fixed stale bind group after a dynamic uniform slot reallocates its buffer.
- `App::on_*(WindowIndex, ..)` 注册**静默失效**（零日志）：当回调先于窗口创建到达时，
  host drain `cb_rx` 时 `window_callbacks.get_mut(handle)` 返回 `None`，整个注册被丢弃。
  根因是 `App::window()` 走 `create_tx`、`on_*` 走 `cb_tx`，**两个独立 channel
  跨 channel 无顺序保证** —— 一轮 `about_to_wait` 里可能「建窗请求还没进（槽 len=0）
  但回调已到」。现在 drain 时按 handle 提前建槽（回调注册本就只依赖 `WindowIndex`，
  与窗口对象何时创建无关），并对超出 `MAX_CALLBACK_SLOTS` 的无效索引告警丢弃。
  **`on_*` 现在可以在 `app.run(..)` 之前正常注册。**
- 无焦点时自动退出挂起：窗口已关闭但进程不退。根因是 winit 0.30 Windows 的
  `exit()` 只置标志不唤醒等待者，而 `run_app` 的退出检查点全在无限 park 返回之后，
  无焦点零事件时 park 永不返回。修为 `exit()` 后自发一次 wake（多余时无害）；
  附带退出握手收敛：Exit ack 确认＋re-wake、`draw_idle` 2s 超时放行、建窗 pending
  backstop 告警、全链路 `VIREO_EXIT_TRACE` 诊断。
  Fixed hang on auto-exit without window focus: window closed but process stayed alive.
  Root cause is winit 0.30 Windows `exit()` setting the flag without waking the waiter
  while `run_app` only checks it after an infinite park; fixed with a self-wake after
  `exit()`, plus exit handshake hardening (ack, timeouts, backstop, trace).

## [0.1.2]

### Fixed / 修复

- vireo-main 结束（正常返回或 panic）即关闭所有窗口并退出进程：
  loop/main panic 后不再留下冻住的窗口与僵尸进程（此前须 taskkill）。
  收尾走逐窗 close 路径（与 X 关闭一致）；手动 `App::run`/`spawn`
  自驱 main 的程序不受影响。
  When vireo-main ends (normally or via panic), all windows are closed and the
  process exits: no more frozen windows or zombie processes after loop/main
  panics (which previously required taskkill). Teardown reuses the per-window
  close path, identical to X-close. Programs driving main manually via
  `App::run`/`spawn` are unaffected.
- 并发双重关窗计数下溢：`alive_window_count` 只在槽位实际由 `Some` 变 `None`
  时递减，否则回绕后退出谓词永久失效。
  Fixed double-close underflow of `alive_window_count`: decrement only when the
  slot actually transitions from `Some` to `None`, otherwise the counter would
  wrap and permanently disable the exit predicate.
- 进程退出码只反映 main 自身的结局：main panic → 1，其余 0
  （loop panic 被 main 妥善处理后正常返回仍是 0）。
  The process exit code reflects only main's own fate: main panic → 1,
  otherwise 0 (a loop panic gracefully handled by main followed by a normal
  return still exits 0).
- 默认窗口图标编译进库：根目录 `logo.png` 经 `include_bytes!` baked，
  不再运行时读启动目录——此前在哪启动决定有没有图标，且失败全程静默。
  `icon_from_path` 任一步失败改 `log::warn!`（路径 + 阶段）。
  The default window icon is now baked into the library (`logo.png` via
  `include_bytes!`) instead of being read from the startup directory at
  runtime — previously icon presence depended on where the process was launched
  from, and all failures were silent. `icon_from_path` now `log::warn!`s
  (path + stage) on any failure.

## [0.1.1]

### Fixed / 修复
- 删除渲染循环内全部调试 `eprintln!` 输出（`[acq]` / `[draw]` / `[resize-trace]`），
  含 `VIREO_DRAW_TRACE` / `VIREO_RESIZE_TRACE` 基础设施；`Validation` 路径保留
  `log::warn!`。0.1.0 中部分 acquire 日志未进 env 门导致每帧刷屏，现已清除。
  Removed all debug `eprintln!` output from the render loop (`[acq]` / `[draw]` /
  `[resize-trace]`), including the `VIREO_DRAW_TRACE` / `VIREO_RESIZE_TRACE`
  machinery. In 0.1.0 some acquire logs bypassed the env gate and printed every
  frame; now fully removed. The `Validation` path keeps its `log::warn!`.

## [0.1.0] — 已 yank / yanked

首个发布版。因含上述调试刷屏问题，不建议使用，请用 0.1.1。
Initial release. Contains the debug-spam issue above; not recommended, use 0.1.1.
