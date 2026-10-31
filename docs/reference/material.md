# 自定义材质（Material ABI）

> 面向 vireo 使用者。目标读者：要写自己的 WGSL 片元着色器（辉光、描边、后处理、多目标输出）的人。
> 适用版本：0.2.0。
>
> **本文件是 Material 契约的唯一来源**；README 只留入口。改这里时同步
> `src/gpu/pipeline.rs` 的 `SHAPE_FRAGMENT_SUPPORT_WGSL` / `MATERIAL_INPUT_WGSL`。

## 1. 最小材质

一份 `fn material_main` 自动同时用于 **shape 和 text**（不用写两套）：

```wgsl
fn material_main(in: MaterialInput) -> vec4<f32> {
    let base = vireo_base_sample(in.base_uv);   // DrawBatch::set_texture 的图
    if in.target_type == 1u {                   // 1 = text
        return vec4<f32>(base.rgb, in.color.a);
    }
    return vec4<f32>(base.rgb * in.color.rgb, in.color.a);
}
```

```rust
let mat = app.material(WGSL)?;      // 编译失败返回 Err（含 WGSL 行号映射），不 panic
batch.set_custom_material(Some(mat));
```

引擎会在这个返回值之上再乘 alpha / 应用 SDF 软边（见 §5 的边界）。

## 2. `MaterialInput` 字段

**优先用注入的 helper，别写死内部 binding 名**（内部名不保证稳定）。

| 字段 / helper | 含义 |
|---|---|
| `in.uv` | 内容自身 UV（shape 图元 / text 的字形图集） |
| `in.base_uv` | batch 基础贴图 UV（`set_texture` + `set_uv`）。text 下**每字形重复**，不是整行连续映射 |
| `in.local_pos` | 局部坐标（逻辑像素）。SDF 距离、渐变都基于它 |
| `in.color` | 引擎默认基色（rgb）；alpha 由包装器在 `material_main` **之后**再乘 |
| `in.target_type` | `0` = shape，`1` = text |
| `in.content_type` | 引擎内部内容分类（做特殊处理时才需要） |
| `in.sdf_params` / `in.sdf_extra` / `in.sdf_type` | SDF 原语参数（自己做距离场时用） |
| `vireo_base_sample(uv)` | 采 batch 基础贴图 |
| `vireo_base_color(in)` | 当前默认基色 |
| `vireo_has_base_sample()` / `vireo_has_local_pos()` / `vireo_has_sdf()` | 能力探针（不同绘制路径下可用的东西不同） |

**GPU 时间**：`camera.time_secs`（秒）在相机 uniform 末尾，引擎每帧写默认钟时间，
CPU 不用改任何 uniform：

```wgsl
struct Camera { projection: mat4x4<f32>, dpi_scale: f32, time_secs: f32 }  // @group(0) @binding(0)
```

需要另一条时间线（暂停 / 倍速 / 定值）时：

```rust
let clock = app.gpu().create_clock();          // 新时间线
app.gpu().set_clock_scale(clock, 0.0);         // 0 = 暂停（恢复时无跳变）
app.gpu().set_clock_time(clock, 3.5);          // 定值（确定性回放）
batch.set_clock(clock);                        // 这个 batch 用它
```

## 3. 资源与 group 布局（两种模式，别混用）

### 模式 A：默认材质（引擎插入 group 0/1/2）

| group | 内容 |
|---|---|
| 0 | 相机（`projection` / `dpi_scale` / `time_secs`），`@binding(0)` |
| 1 | 引擎纹理 + sampler（基础贴图） |
| 2 | transform 表、多边形/折线边数据、SDF 实例数据、折线色带（都是 storage） |
| **3** | **用户 material**：1024B storage（`MATERIAL_UNIFORM_SIZE`）+ 最多 4 组 tex/sampler |

shape 与 text 共用这套 ABI。

### 模式 B：自管材质（引擎**不插入**任何 group）

```rust
// 用户提供 (group_index, BGL) 列表；自己声明 @group/@binding
let mat = app.material_self_managed(WGSL, &[(0, &my_bgl), (1, &my_bgl2)])?;
```

适合把已有 wgpu shader 零改移植。**注意**：自管模式下 `in.base_uv` / `vireo_base_sample`
这些 helper 不可用（没有引擎纹理组），要自己传纹理。

### 只换管线布局

`material_manual(source, &bgl)` / `material_with_resources(source, MaterialResources { .. })`
用于"只要引擎帮我建管线，资源我自己管"。支持自定义顶点着色器的场景见
§6（必须显式声明 `ShapeVertexLayout`）。

## 4. MRT：一次提交写多张颜色附件

把返回值改成 `MaterialOutput`（第 0 个字段**必须**命名为 `color`，引擎在其上应用
alpha / SDF；其余 `@location` 原样输出）：

```wgsl
struct MaterialOutput {
    @location(0) color: vec4<f32>,   // 引擎会处理
    @location(1) aux: vec4<f32>,     // 你自己写
}
fn material_main(in: MaterialInput) -> MaterialOutput {
    var out: MaterialOutput;
    out.color = vec4<f32>(1.0, 0.0, 0.0, 1.0);
    out.aux = vec4<f32>(0.0, 0.0, 1.0, 1.0);
    return out;
}
```

```rust
win.draw_mrt(Some(clear), &extra_targets, &[&batch]);   // 逐目标 Blend，一次提交
```

引擎自带的形状/文字**也能**画进 MRT pass：它们只写第 0 个目标，其余附件保持
`LoadOp::Load` 的载入值（不会被清零）；反过来，多输出的材质也能用在单目标 pass。

## 5. 边界（这些地方材质不等于万能）

- **alpha 与 SDF 软边由引擎在 `material_main` 之后应用**，你返回的 alpha 是"内容 alpha"。
- **per-sample AA 只超采样 SDF 几何覆盖度**：形状画成整像素四边形，MSAA 采样点覆盖率对它无效；
  纹理采样与屏空间后处理在 `PerSample` 档**不**受益（`uv` 仍逐像素插值）。
- **自定义 shape 顶点着色器在所有 AA 档都按像素求值**（引擎统一 `(linear)`），
  不会因 AA 档编译出不同变体。
- **约定未冻结**：`MaterialInput` 会加字段（只加不改），内部 binding 名不保证稳定 ——
  所以请用 helper，不要手写 `@group(2) @binding(1)` 这类内部名。

## 6. 自定义顶点着色器

三条绘制路径的顶点输入契约互不兼容，必须显式声明跑在哪条上：

```rust
let mat = app.material_with_vertex_shader(FRAG_WGSL, VERT_WGSL, ShapeVertexLayout::SdfInstance)?;
```

| 布局 | 顶点缓冲 | 说明 |
|---|---|---|
| `Mesh` | `Vertex`（68B，`@location(0..7)`） | 直接 push 顶点 / Area |
| `SdfInstance` | 8B `QuadVertex` + 112B `InstanceData`（vertex-pulling，按 `instance_index` 读） | 默认形状路径 |
| `GeoInstance` | 16B `GeoVertex` + `GeoInstance` | 几何模板路径 |
| `Particle` | 16B `QuadVertex` + 152B `ParticleInstance` | 粒子 |

声明之外的绘制路径会被**跳过并 warn-once**（不回落 mesh，避免"类型匹配、语义全错"的管线）。

## 7. 深度 / 模板

```rust
mat.set_depth_write(true);
mat.set_depth_compare(wgpu::CompareFunction::LessEqual);   // 不是 Option
win.draw_with_depth_stencil(target, &[&batch]);            // 深度预通道、跨 batch 深度保留
```

外部 DS view 走 `RenderTarget::depth_stencil`；MRT + stencil + MSAA 的组合在
`gpu_bench` 里有覆盖用例。
