//! GPU-driven 粒子：spawn 参数烘焙 + VS 按 time 积分。
//!
//! - 状态放用户侧 [`ParticlePool`]（稳定槽位，跨帧复用）；`DrawBatch` 只做帧内 record。
//! - [`draw_particles`] 把存活集一次 bulk 推送为 [`ParticleInstance`]（`src/gpu/mod.rs`），
//!   VS 按 `camera.time` 算 `pos += vel*age` + fade 包络，存活期 CPU 零更新。
//! - 复用 batch 当前贴图（一次调用单贴图，跨贴图分多次调用）与当前 transform
//!   （`batch.view` 左乘自动覆盖，谱面滚动免费）。
//! - 带 custom VS 的材质不能消费 instance ABI：此时烘焙为静态 mesh quad
//!   （出生位形），custom VS 可自行位移；fade/积分请用材料 uniform 自管。
//! - `to_area()` 不含粒子（时变内容不定义裁剪区）；culling 用出生框＋有限寿命终点框，
//!   不朽且运动的粒子请用手动 bounds。

use crate::gpu::ParticleInstance;
use crate::render::DrawBatch;

/// CPU 侧 spawn 参数（[`ParticlePool`] 存此结构，GPU 镜像见 [`ParticleInstance`]）。
#[derive(Copy, Clone, Debug)]
pub struct Particle {
    /// 出生位置（world/逻辑像素，record 时世界坐标）。
    pub pos: [f32; 2],
    /// 速度（px/s，GPU 积分 `pos += vel*age`）。
    pub vel: [f32; 2],
    /// 半尺寸 wh（quad 半宽/半高）。
    pub size: [f32; 2],
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

/// 用户侧粒子池：稳定槽位 + 自由表，跨帧持有，帧内 [`draw_particles`] 一次推送。
pub struct ParticlePool {
    entries: Vec<SlotEntry>,
    free: Vec<u32>,
    live_count: usize,
}

impl ParticlePool {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            free: Vec::new(),
            live_count: 0,
        }
    }

    /// 存活数。
    pub fn live_count(&self) -> usize {
        self.live_count
    }

    /// 生成粒子，返回稳定槽位。
    pub fn spawn(&mut self, p: Particle) -> ParticleSlot {
        if let Some(index) = self.free.pop() {
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
        }
    }

    /// 原地更新存活粒子。槽位过期/已杀返回 `false`（不做任何事）。
    pub fn update(&mut self, slot: ParticleSlot, p: Particle) -> bool {
        match self.entries.get_mut(slot.index as usize) {
            Some(entry) if entry.live && entry.generation == slot.generation => {
                entry.particle = p;
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

    /// 清空全部（容量保留）。
    pub fn clear(&mut self) {
        self.entries.clear();
        self.free.clear();
        self.live_count = 0;
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
    batch
        .particles
        .extend(pool.live().map(|(_, p)| ParticleInstance {
            pos_size: [p.pos[0], p.pos[1], p.size[0].max(0.0), p.size[1].max(0.0)],
            vel_time: [p.vel[0], p.vel[1], p.birth, p.life],
            color: [p.color.r, p.color.g, p.color.b, p.color.a],
            uv_rect: p.uv_rect,
            fade_misc: [p.fade_in.max(0.0), p.fade_out.max(0.0), p.seed, 0.0],
            transform_index,
            _padding: 0,
        }));
    batch.record_particle_command(start);
}

/// custom VS 回退：静态出生位形烘焙为 mesh quad（4v/6i each，sdf_type=0）。
/// 动画丢失是预期的——custom VS 持有顶点后可用材料 uniform 自管时间。
fn bake_particles_mesh(batch: &mut DrawBatch, pool: &ParticlePool) {
    let transform_index = batch.current_transform_index();
    let ndx_start = batch.indices.len() as u32;
    for (_, p) in pool.live() {
        let (px, py, hw, hh) = (p.pos[0], p.pos[1], p.size[0].max(0.0), p.size[1].max(0.0));
        let [u0, v0, u1, v1] = p.uv_rect;
        let base = batch.vertices.len() as u32;
        for (x, y, u, v) in [
            (px - hw, py - hh, u0, v0),
            (px + hw, py - hh, u1, v0),
            (px + hw, py + hh, u1, v1),
            (px - hw, py + hh, u0, v1),
        ] {
            batch.vertices.push(crate::gpu::Vertex::new_uv_xform(
                x,
                y,
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
            size: [5.0, 5.0],
            color: WHITE,
            uv_rect: [0.0, 0.0, 1.0, 1.0],
            birth: 0.0,
            life: 10.0,
            fade_in: 1.0,
            fade_out: 1.0,
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
        assert_eq!(inst.transform_index, 0);
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
