//! GPU-driven 粒子：spawn 参数烘焙 + VS 按 time 积分。
//!
//! - 状态放用户侧 [`ParticlePool`]（稳定槽位，跨帧复用）；`DrawBatch` 只做帧内 record。
//! - [`draw_particles`] 把存活集一次 bulk 推送为 [`ParticleInstance`]（`src/gpu/mod.rs`），
//!   VS 按 `camera.time` 算 `pos += vel*age` + fade 包络，存活期 CPU 零更新。
//! - 复用 batch 当前贴图（一次调用单贴图，跨贴图分多次调用）与当前 transform
//!   （`batch.view` 左乘自动覆盖，谱面滚动免费）。
//! - 带 custom VS 的材质不能消费 instance ABI：此时烘焙为静态 mesh quad
//!   （出生位形），custom VS 可自行位移；fade/积分请用材料 uniform 自管。
//! - `to_area()` 不含粒子（时变内容不定义裁剪区）；culling 用出生框＋有限寿命终点框
//!   （朝向按外接圆保守），不朽且运动的粒子请用手动 bounds。
//! - `sweep` 请手动按 cadence 调（每 15–30 帧一次；代价 O(entries 含 dead)，
//!   entries 靠 freelist 复用不缩；`draw_particles` 拿不到时钟，不做 auto-sweep）。

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use crate::gpu::ParticleInstance;
use crate::render::DrawBatch;

/// 图集序列（行优先精灵表动画）：`uv_rect` 当整表矩形，按 `cols×rows` 切分，
/// `frame = (floor(age*fps) + start) % (cols*rows)`；row 0 = `uv_rect` 的 v0 边。
/// 禁用约定（`Particle.atlas = None` 烘焙零值）：`cols*rows <= 1` 或 `fps <= 0` 即静态。
#[derive(Copy, Clone, Debug)]
pub struct AtlasSeq {
    /// 列数。
    pub cols: u32,
    /// 行数。
    pub rows: u32,
    /// 帧率（帧/秒）。
    pub fps: f32,
    /// 起始帧偏移。
    pub start: u32,
}

/// CPU 侧 spawn 参数（[`ParticlePool`] 存此结构，GPU 镜像见 [`ParticleInstance`]）。
#[derive(Copy, Clone, Debug)]
pub struct Particle {
    /// 出生位置（world/逻辑像素，record 时世界坐标）。
    pub pos: [f32; 2],
    /// 速度（px/s，GPU 积分 `pos += vel*age`）。
    pub vel: [f32; 2],
    /// 阻尼指数（原始指数语义：`dv/dt = damping*v`；**正数=指数增长，衰减请填负数**；
    /// 默认 0.0 = 无阻尼，今日行为）。
    pub damping: f32,
    /// 恒定重力加速度（px/s²，默认 `[0,0]` = 无重力）。
    pub gravity: [f32; 2],
    /// 半尺寸 wh（quad 半宽/半高）。
    pub size: [f32; 2],
    /// 网格朝向弧度（只转网格，不转速度；默认 0.0 = 今日行为）。
    pub rotation: f32,
    /// 基色 rgba（a = 峰值 alpha，fade 包络乘在此之上）。
    pub color: crate::color::Color,
    /// 图集矩形（u0,v0,u1,v1，绝对坐标，不受 batch `set_uv` 重映射）。
    pub uv_rect: [f32; 4],
    /// 出生时间（秒，与 `camera.time` 同一时钟，见 `GpuContext::clock_time`）。
    pub birth: f32,
    /// 寿命秒（`<= 0` = 不朽，跳过 fade-out）。
    pub life: f32,
    /// 淡入秒（`<= 0` = 瞬时出现）。
    pub fade_in: f32,
    /// 淡出秒（`<= 0` = 到 life 时硬切；寿命内按 `life - fade_out → life` 包络）。
    pub fade_out: f32,
    /// 尺寸终点倍率（`size *= mix(1.0, size_mul_end, k)`，`k = clamp(age/life)` 线性；
    /// 默认 1.0 = 无变化；0.0 = 收缩到点；不朽粒子 k 恒 0）。
    pub size_mul_end: f32,
    /// 颜色终点（`rgb = mix(color.rgb, end.rgb, k)`；alpha 仍只由包络 × `color.a` 决定，
    /// `end.a` 忽略；默认 None = 只走 alpha 包络）。
    pub color_end: Option<crate::color::Color>,
    /// 图集序列（默认 None = 静态，用 `uv_rect` 原样）。
    pub atlas: Option<AtlasSeq>,
    /// 每粒子随机种子（供材料/变体用，VS 透传不用）。
    pub seed: f32,
}

/// 粒子槽位（池内下标 + 代数；stale kill/update 返回 `false`）。
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ParticleSlot {
    index: u32,
    generation: u32,
}

struct SlotEntry {
    particle: Particle,
    generation: u32,
    live: bool,
}

/// 池满时的溢出策略（`set_max_live` 生效时）。
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum OverflowPolicy {
    /// 丢弃新粒子：`spawn` 返回死槽（`update`/`kill` 恒 false），`try_spawn` 返回 None。
    DropNewest,
    /// 杀最老（birth 最小）腾槽：`spawn`/`try_spawn` 照常成功。堆维护，摊销 O(log n)。
    KillOldest,
}

/// 用户侧粒子池：稳定槽位 + 自由表，跨帧持有，帧内 [`draw_particles`] 一次推送。
pub struct ParticlePool {
    entries: Vec<SlotEntry>,
    free: Vec<u32>,
    live_count: usize,
    max_live: Option<usize>,
    policy: OverflowPolicy,
    /// 最老堆：`(birth_bits, generation, index)` 最小堆。kill/sweep/update 不删堆内条目，
    /// 靠快照校验懒删除（generation 或 birth 对不上即 stale）；`update` 改 birth 时补推新条目；
    /// 堆过长按阈值重建（见 `maybe_rebuild_heap`）。
    oldest: BinaryHeap<Reverse<(u32, u32, u32)>>,
}

impl ParticlePool {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            free: Vec::new(),
            live_count: 0,
            max_live: None,
            policy: OverflowPolicy::DropNewest,
            oldest: BinaryHeap::new(),
        }
    }

    /// 存活数。
    pub fn live_count(&self) -> usize {
        self.live_count
    }

    /// 存活上限（默认 None = 不限，今日行为）。只约束未来 `spawn`，不追溯杀已有。
    /// None→Some 时按现存活集建堆（O(n log n)，一次性）；Some→None 时清堆放内存。
    pub fn set_max_live(&mut self, cap: Option<usize>) {
        if cap.is_some() && self.max_live.is_none() {
            let items: Vec<_> = self
                .live()
                .map(|(slot, p)| Reverse((p.birth.to_bits(), slot.generation, slot.index)))
                .collect();
            self.oldest.clear();
            self.oldest.extend(items);
        } else if cap.is_none() {
            self.oldest.clear();
        }
        self.max_live = cap;
    }

    /// 当前上限。
    pub fn max_live(&self) -> Option<usize> {
        self.max_live
    }

    /// 满时策略（默认 `DropNewest`）。
    pub fn set_overflow_policy(&mut self, policy: OverflowPolicy) {
        self.policy = policy;
    }

    /// 当前策略。
    pub fn overflow_policy(&self) -> OverflowPolicy {
        self.policy
    }

    /// 生成粒子，返回稳定槽位。满＋`DropNewest` 时返回死槽（index `u32::MAX`，
    /// `update`/`kill` 对其恒 false，`live()` 永不产出——实际是"没入池"）。
    pub fn spawn(&mut self, p: Particle) -> ParticleSlot {
        if let Some(cap) = self.max_live {
            if self.live_count >= cap {
                // cap 为 0 时杀一插一永不收敛，直接全拒（与策略无关）。
                if cap == 0 {
                    return ParticleSlot {
                        index: u32::MAX,
                        generation: 0,
                    };
                }
                match self.policy {
                    OverflowPolicy::DropNewest => {
                        return ParticleSlot {
                            index: u32::MAX,
                            generation: 0,
                        };
                    }
                    OverflowPolicy::KillOldest => {
                        if !self.kill_oldest() {
                            return ParticleSlot {
                                index: u32::MAX,
                                generation: 0,
                            };
                        }
                    }
                }
            }
        }
        let slot = if let Some(index) = self.free.pop() {
            let entry = &mut self.entries[index as usize];
            entry.particle = p;
            entry.live = true;
            self.live_count += 1;
            ParticleSlot {
                index,
                generation: entry.generation,
            }
        } else {
            let index = self.entries.len() as u32;
            self.entries.push(SlotEntry {
                particle: p,
                generation: 0,
                live: true,
            });
            self.live_count += 1;
            ParticleSlot {
                index,
                generation: 0,
            }
        };
        // 不限速池不维护堆（纯浪费＋内存涨）；set_max_live 建堆，kill_oldest 只读堆。
        if self.max_live.is_some() {
            self.oldest
                .push(Reverse((p.birth.to_bits(), slot.generation, slot.index)));
            self.maybe_rebuild_heap();
        }
        slot
    }

    /// 诚实版 `spawn`：满＋`DropNewest` 时返回 None（不给死槽）。
    /// 满＋`KillOldest` 时杀最老后总能成功（cap 为 0 时也返回 None）。
    pub fn try_spawn(&mut self, p: Particle) -> Option<ParticleSlot> {
        let slot = self.spawn(p);
        if slot.index == u32::MAX {
            None
        } else {
            Some(slot)
        }
    }

    /// 原地更新存活粒子。槽位过期/已杀返回 `false`（不做任何事）。
    /// birth 变化且限速时补推新条目重 key（O(log n)）；birth 不变不碰堆。
    pub fn update(&mut self, slot: ParticleSlot, p: Particle) -> bool {
        match self.entries.get_mut(slot.index as usize) {
            Some(entry) if entry.live && entry.generation == slot.generation => {
                let rekey =
                    self.max_live.is_some() && p.birth.to_bits() != entry.particle.birth.to_bits();
                entry.particle = p;
                if rekey {
                    self.oldest
                        .push(Reverse((p.birth.to_bits(), slot.generation, slot.index)));
                }
                true
            }
            _ => false,
        }
    }

    /// 杀死粒子（幂等：重复 kill 返回 `false`，不 double-free）。
    pub fn kill(&mut self, slot: ParticleSlot) -> bool {
        match self.entries.get_mut(slot.index as usize) {
            Some(entry) if entry.live && entry.generation == slot.generation => {
                entry.live = false;
                entry.generation = entry.generation.wrapping_add(1);
                self.free.push(slot.index);
                self.live_count -= 1;
                true
            }
            _ => false,
        }
    }

    /// 杀最老存活粒子（birth 最小；并列时 index 小优先）。堆顶 stale 条目
    /// （已杀/sweep/代数或 birth 对不上）直接丢弃。成功返回 true。
    fn kill_oldest(&mut self) -> bool {
        while let Some(Reverse((birth_bits, generation, index))) = self.oldest.pop() {
            let valid = match self.entries.get(index as usize) {
                Some(entry)
                    if entry.live
                        && entry.generation == generation
                        && entry.particle.birth.to_bits() == birth_bits =>
                {
                    true
                }
                _ => false,
            };
            if valid {
                return self.kill(ParticleSlot { index, generation });
            }
        }
        false
    }

    /// 堆过长时按存活集重建（stale 条目清零）。阈值 `2*entries + 64`，
    /// 每次 `spawn` O(1) 检查，重建本身 O(live)。
    fn maybe_rebuild_heap(&mut self) {
        if self.oldest.len() > self.entries.len() * 2 + 64 {
            let items: Vec<_> = self
                .live()
                .map(|(slot, p)| Reverse((p.birth.to_bits(), slot.generation, slot.index)))
                .collect();
            self.oldest.clear();
            self.oldest.extend(items);
        }
    }

    /// 清空全部（容量保留，堆与上限设置保留）。
    pub fn clear(&mut self) {
        self.entries.clear();
        self.free.clear();
        self.live_count = 0;
        self.oldest.clear();
    }

    /// 单遍清除过期有限寿命粒子（`now` 取粒子时钟），返回清除数。
    /// 比 live-迭代＋collect＋逐个 kill 少一遍全量扫描和一次分配；
    /// 不朽粒子与未出生粒子不受影响。
    pub fn sweep(&mut self, now: f32) -> usize {
        let mut killed = 0;
        for (i, entry) in self.entries.iter_mut().enumerate() {
            if entry.live
                && entry.particle.life > 0.0
                && now > entry.particle.birth + entry.particle.life
            {
                entry.live = false;
                entry.generation = entry.generation.wrapping_add(1);
                self.free.push(i as u32);
                self.live_count -= 1;
                killed += 1;
            }
        }
        killed
    }

    /// 存活粒子迭代（`(slot, &Particle)`，槽位顺序）。
    pub fn live(&self) -> impl Iterator<Item = (ParticleSlot, &Particle)> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.live)
            .map(|(i, e)| {
                (
                    ParticleSlot {
                        index: i as u32,
                        generation: e.generation,
                    },
                    &e.particle,
                )
            })
    }
}

impl Default for ParticlePool {
    fn default() -> Self {
        Self::new()
    }
}

/// CPU 侧闭式位移（与 VS 同式，用于 culling 终点框；`t` 取寿命）。
/// 与 WGSL 同分段：无量纲 `q = damping*t`，`|q| < 0.01` 走 Taylor。
pub(crate) fn integrate_travel(vel: [f32; 2], damping: f32, gravity: [f32; 2], t: f32) -> [f32; 2] {
    let q = damping * t;
    let (k1, k2) = if q.abs() < 0.01 {
        (
            t * (1.0 + q * (0.5 + q / 6.0)),
            t * t * (0.5 + q * (1.0 / 6.0 + q / 24.0)),
        )
    } else {
        let e = q.exp();
        ((e - 1.0) / damping, (e - 1.0 - q) / (damping * damping))
    };
    [vel[0] * k1 + gravity[0] * k2, vel[1] * k1 + gravity[1] * k2]
}

/// 把池内存活集一次推送进 batch（`ParticleInstance` bulk 扩展 + 一条合并命令）。
///
/// - 用 batch 当前贴图与当前 transform（`transform_index` 逐粒子相同则自动去重，
///   整层粒子通常只占 1 个 transform 槽）。
/// - 空池直接返回（不写命令，不断段）。
/// - custom VS 材质下烘焙静态 mesh quad（见模块文档），不走 instance 路径。
/// - `uv_rect` 取粒子自带绝对坐标，不受 batch `set_uv` 影响。
/// - fade 包络参数钳为非负（WGSL `smoothstep` edge 必须有序）。
pub fn draw_particles(batch: &mut DrawBatch, pool: &ParticlePool) {
    if pool.live_count() == 0 {
        return;
    }
    if batch
        .custom_material()
        .is_some_and(|m| m.has_custom_vertex_shader())
    {
        bake_particles_mesh(batch, pool);
        return;
    }
    let transform_index = batch.current_transform_index();
    let start = batch.particles.len() as u32;
    batch.particles.extend(pool.live().map(|(_, p)| {
        let (end_rgb, has_end) = match p.color_end {
            Some(e) => ([e.r, e.g, e.b], 1.0),
            None => ([0.0, 0.0, 0.0], 0.0),
        };
        ParticleInstance {
            pos_size: [p.pos[0], p.pos[1], p.size[0].max(0.0), p.size[1].max(0.0)],
            vel_time: [p.vel[0], p.vel[1], p.birth, p.life],
            color: [p.color.r, p.color.g, p.color.b, p.color.a],
            uv_rect: p.uv_rect,
            fade_misc: [p.fade_in.max(0.0), p.fade_out.max(0.0), p.seed, has_end],
            motion2: [p.damping, p.gravity[0], p.gravity[1], p.rotation],
            end_rgba_size: [end_rgb[0], end_rgb[1], end_rgb[2], p.size_mul_end],
            atlas_seq: match p.atlas {
                Some(a) => [a.cols as f32, a.rows as f32, a.fps, a.start as f32],
                None => [0.0, 0.0, 0.0, 0.0],
            },
            transform_index,
            _padding: 0,
        }
    }));
    batch.record_particle_command(start);
}

/// custom VS 回退：静态出生位形烘焙为 mesh quad（4v/6i each，sdf_type=0）。
/// 动画丢失是预期的——custom VS 持有顶点后可用材料 uniform 自管时间。
/// 朝向是静态量可精确烘焙；阻尼/重力/尺寸颜色终点/序列动画只留出生快照。
fn bake_particles_mesh(batch: &mut DrawBatch, pool: &ParticlePool) {
    let transform_index = batch.current_transform_index();
    let ndx_start = batch.indices.len() as u32;
    for (_, p) in pool.live() {
        let (px, py, hw, hh) = (p.pos[0], p.pos[1], p.size[0].max(0.0), p.size[1].max(0.0));
        let [u0, v0, u1, v1] = p.uv_rect;
        let base = batch.vertices.len() as u32;
        let cs = p.rotation.cos();
        let sn = p.rotation.sin();
        for (x, y, u, v) in [
            (-hw, -hh, u0, v0),
            (hw, -hh, u1, v0),
            (hw, hh, u1, v1),
            (-hw, hh, u0, v1),
        ] {
            batch.vertices.push(crate::gpu::Vertex::new_uv_xform(
                px + x * cs - y * sn,
                py + x * sn + y * cs,
                u,
                v,
                p.color,
                transform_index,
            ));
        }
        batch
            .indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    batch.record_mesh_command(ndx_start, false);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::colors::WHITE;
    use crate::render::DrawBatch;

    fn test_particle() -> Particle {
        Particle {
            pos: [10.0, 10.0],
            vel: [1.0, 0.0],
            damping: 0.0,
            gravity: [0.0, 0.0],
            size: [5.0, 5.0],
            rotation: 0.0,
            color: WHITE,
            uv_rect: [0.0, 0.0, 1.0, 1.0],
            birth: 0.0,
            life: 10.0,
            fade_in: 1.0,
            fade_out: 1.0,
            size_mul_end: 1.0,
            color_end: None,
            atlas: None,
            seed: 0.5,
        }
    }

    #[test]
    fn pool_spawn_kill_reuses_slot_with_new_generation() {
        let mut pool = ParticlePool::new();
        assert_eq!(pool.live_count(), 0);
        let s0 = pool.spawn(test_particle());
        assert_eq!(pool.live_count(), 1);
        assert!(pool.kill(s0));
        assert_eq!(pool.live_count(), 0);
        assert!(!pool.kill(s0));
        assert!(!pool.update(s0, test_particle()));
        let s1 = pool.spawn(test_particle());
        assert_eq!(pool.live_count(), 1);
        assert_ne!(s0, s1);
        assert!(pool.update(s1, test_particle()));
        assert_eq!(pool.live().count(), 1);
    }

    #[test]
    fn pool_clear_resets() {
        let mut pool = ParticlePool::new();
        pool.spawn(test_particle());
        pool.spawn(test_particle());
        pool.clear();
        assert_eq!(pool.live_count(), 0);
        assert_eq!(pool.live().count(), 0);
        let s = pool.spawn(test_particle());
        assert!(pool.update(s, test_particle()));
    }

    #[test]
    fn sweep_kills_expired_only() {
        let mut pool = ParticlePool::new();
        pool.spawn(test_particle());
        let mut immortal = test_particle();
        immortal.life = -1.0;
        pool.spawn(immortal);
        let mut unborn = test_particle();
        unborn.birth = 100.0;
        pool.spawn(unborn);
        assert_eq!(pool.sweep(5.0), 0);
        assert_eq!(pool.live_count(), 3);
        assert_eq!(pool.sweep(11.0), 1);
        assert_eq!(pool.live_count(), 2);
        assert_eq!(pool.sweep(200.0), 1);
        assert_eq!(pool.live_count(), 1);
    }

    #[test]
    fn draw_particles_empty_pool_writes_no_command() {
        let mut batch = DrawBatch::new();
        let pool = ParticlePool::new();
        let before = batch.shape_commands.len();
        draw_particles(&mut batch, &pool);
        assert!(batch.particles.is_empty());
        assert_eq!(batch.shape_commands.len(), before);
    }

    #[test]
    fn draw_particles_records_merged_command_with_stats() {
        let mut batch = DrawBatch::new();
        let mut pool = ParticlePool::new();
        pool.spawn(test_particle());
        pool.spawn(test_particle());
        draw_particles(&mut batch, &pool);
        draw_particles(&mut batch, &pool);
        assert_eq!(batch.particles.len(), 4);
        assert_eq!(batch.shape_stats().particles, 4);
        assert_eq!(batch.shape_vertex_count(), 16);
        let particle_cmds = batch
            .shape_commands
            .iter()
            .filter(|c| matches!(c, crate::render::BatchShapeCommand::Particles { .. }))
            .count();
        assert_eq!(particle_cmds, 1);
        assert!(batch.shape_commands_valid());
        // 实例内容：首粒子参数逐字段正确
        let inst = &batch.particles[0];
        assert_eq!(inst.pos_size, [10.0, 10.0, 5.0, 5.0]);
        assert_eq!(inst.vel_time, [1.0, 0.0, 0.0, 10.0]);
        assert_eq!(inst.color, [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(inst.fade_misc, [1.0, 1.0, 0.5, 0.0]);
        assert_eq!(inst.motion2, [0.0, 0.0, 0.0, 0.0]);
        assert_eq!(inst.transform_index, 0);
    }

    #[test]
    fn draw_particles_bakes_rotation_into_motion2() {
        let mut batch = DrawBatch::new();
        let mut pool = ParticlePool::new();
        let mut p = test_particle();
        p.rotation = 1.25;
        pool.spawn(p);
        draw_particles(&mut batch, &pool);
        assert_eq!(batch.particles[0].motion2[3], 1.25);
    }

    #[test]
    fn integrate_travel_matches_closed_form_and_falls_back() {
        // 无阻尼无重力 = 匀速
        assert_eq!(
            integrate_travel([3.0, -2.0], 0.0, [0.0, 0.0], 4.0),
            [12.0, -8.0]
        );
        // 纯重力 = 抛物线
        let g = integrate_travel([0.0, 0.0], 0.0, [0.0, 400.0], 1.0);
        assert!((g[1] - 200.0).abs() < 1e-4);
        // 分段边界两侧连续（q=±0.01；k2 的 f32 绝对误差会被 g 放大，看相对）
        for d in [0.005f32, -0.005, 0.02, -0.02] {
            let t = 0.01 / d.abs();
            let a = integrate_travel([10.0, 5.0], d * 0.999, [3.0, 9.0], t);
            let b = integrate_travel([10.0, 5.0], d * 1.001, [3.0, 9.0], t);
            for i in 0..2 {
                let rel = (a[i] - b[i]).abs() / b[i].abs().max(1.0);
                assert!(rel < 2e-3, "d={d} boundary jump {a:?} vs {b:?}");
            }
        }
        // 负阻尼衰减：位移小于匀速
        let dec = integrate_travel([10.0, 0.0], -2.0, [0.0, 0.0], 1.0);
        assert!((dec[0] - 4.323).abs() < 1e-2 && dec[0] < 10.0);
    }

    #[test]
    fn draw_particles_bakes_endpoints_and_flag() {
        use crate::color::colors::RED;
        let mut batch = DrawBatch::new();
        let mut pool = ParticlePool::new();
        let mut p = test_particle();
        p.size_mul_end = 0.25;
        p.color_end = Some(RED);
        pool.spawn(p);
        pool.spawn(test_particle());
        draw_particles(&mut batch, &pool);
        assert_eq!(batch.particles[0].end_rgba_size, [0.9, 0.16, 0.22, 0.25]);
        assert_eq!(batch.particles[0].fade_misc[3], 1.0);
        assert_eq!(batch.particles[1].end_rgba_size, [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(batch.particles[1].fade_misc[3], 0.0);
    }

    #[test]
    fn draw_particles_bakes_atlas_seq_or_zeros() {
        let mut batch = DrawBatch::new();
        let mut pool = ParticlePool::new();
        let mut p = test_particle();
        p.atlas = Some(AtlasSeq {
            cols: 4,
            rows: 2,
            fps: 12.0,
            start: 3,
        });
        pool.spawn(p);
        pool.spawn(test_particle());
        draw_particles(&mut batch, &pool);
        assert_eq!(batch.particles[0].atlas_seq, [4.0, 2.0, 12.0, 3.0]);
        assert_eq!(batch.particles[1].atlas_seq, [0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn cap_drop_newest_returns_dead_slot() {
        let mut pool = ParticlePool::new();
        pool.set_max_live(Some(2));
        pool.spawn(test_particle());
        pool.spawn(test_particle());
        assert_eq!(pool.live_count(), 2);
        let dead = pool.spawn(test_particle());
        assert_eq!(pool.live_count(), 2);
        assert!(!pool.kill(dead));
        assert!(!pool.update(dead, test_particle()));
        assert_eq!(pool.live().count(), 2);
        assert!(pool.try_spawn(test_particle()).is_none());
        pool.set_max_live(None);
        assert!(pool.try_spawn(test_particle()).is_some());
    }

    #[test]
    fn cap_kill_oldest_kills_min_birth() {
        let mut pool = ParticlePool::new();
        pool.set_max_live(Some(2));
        pool.set_overflow_policy(OverflowPolicy::KillOldest);
        let mut old = test_particle();
        old.birth = 1.0;
        let mut mid = test_particle();
        mid.birth = 5.0;
        let s_old = pool.spawn(old);
        pool.spawn(mid);
        let mut new = test_particle();
        new.birth = 9.0;
        let s_new = pool.spawn(new);
        assert_eq!(pool.live_count(), 2);
        assert!(!pool.update(s_old, test_particle()));
        assert!(pool.update(s_new, new));
        let mut births: Vec<f32> = pool.live().map(|(_, p)| p.birth).collect();
        births.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert_eq!(births, vec![5.0, 9.0]);
    }

    #[test]
    fn cap_does_not_retro_kill_and_zero_cap_drops_all() {
        let mut pool = ParticlePool::new();
        pool.spawn(test_particle());
        pool.spawn(test_particle());
        pool.spawn(test_particle());
        pool.set_max_live(Some(2));
        assert_eq!(pool.live_count(), 3);
        pool.set_overflow_policy(OverflowPolicy::KillOldest);
        pool.spawn(test_particle());
        assert_eq!(pool.live_count(), 3);
        pool.set_max_live(Some(0));
        assert!(pool.try_spawn(test_particle()).is_none());
    }

    #[test]
    fn kill_oldest_skips_stale_and_honors_update_rekey() {
        let mut pool = ParticlePool::new();
        pool.set_max_live(Some(2));
        pool.set_overflow_policy(OverflowPolicy::KillOldest);
        let mut a = test_particle();
        a.birth = 1.0;
        let mut b = test_particle();
        b.birth = 2.0;
        let sa = pool.spawn(a);
        let sb = pool.spawn(b);
        assert!(pool.kill(sa)); // 堆顶变 stale
        let mut c = test_particle();
        c.birth = 3.0;
        let sc = pool.spawn(c);
        let mut d = test_particle();
        d.birth = 4.0;
        pool.spawn(d); // 满 → 跳过 stale a，杀 b
        assert!(!pool.update(sb, test_particle()));
        assert!(pool.update(sc, test_particle()));
        // update 改 birth 更老 → 补推重 key，下次杀 c
        let mut c_old = test_particle();
        c_old.birth = 0.5;
        assert!(pool.update(sc, c_old));
        let mut e = test_particle();
        e.birth = 5.0;
        pool.spawn(e);
        assert!(!pool.update(sc, test_particle()));
        let mut births: Vec<f32> = pool.live().map(|(_, p)| p.birth).collect();
        births.sort_by(|x, y| x.partial_cmp(y).unwrap());
        assert_eq!(births, vec![4.0, 5.0]);
    }

    #[test]
    fn set_max_live_after_spawns_builds_heap() {
        let mut pool = ParticlePool::new();
        let mut a = test_particle();
        a.birth = 3.0;
        let mut b = test_particle();
        b.birth = 1.0;
        let mut c = test_particle();
        c.birth = 2.0;
        pool.spawn(a);
        pool.spawn(b);
        pool.spawn(c);
        // 不限速时 spawn 不建堆；此刻设限应按现存活集建堆
        pool.set_max_live(Some(2));
        pool.set_overflow_policy(OverflowPolicy::KillOldest);
        let mut d = test_particle();
        d.birth = 4.0;
        pool.spawn(d);
        let mut births: Vec<f32> = pool.live().map(|(_, p)| p.birth).collect();
        births.sort_by(|x, y| x.partial_cmp(y).unwrap());
        assert_eq!(births, vec![2.0, 3.0, 4.0]);
    }

    #[test]
    fn draw_particles_clamps_negative_fade_and_size() {
        let mut batch = DrawBatch::new();
        let mut pool = ParticlePool::new();
        let mut p = test_particle();
        p.fade_in = -2.0;
        p.fade_out = -3.0;
        p.size = [-4.0, -4.0];
        pool.spawn(p);
        draw_particles(&mut batch, &pool);
        let inst = &batch.particles[0];
        assert_eq!(inst.fade_misc[0], 0.0);
        assert_eq!(inst.fade_misc[1], 0.0);
        assert_eq!(inst.pos_size[2], 0.0);
        assert_eq!(inst.pos_size[3], 0.0);
    }
}
