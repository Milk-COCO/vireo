//! GPU 计算管线（P5）。
//!
//! vireo 的渲染路径全部是 graphics pipeline；本模块提供最小可用的 compute 入口：
//! 创建 compute pipeline（用户自带 BGL 与 group 编号，复用 P3 的 group 模型）+
//! 提交一次 dispatch。dispatch 的写入结果对**后续** render pass 可见 —— 每次
//! dispatch 独立提交一个 command buffer，wgpu 在提交边界自动插入 barrier。
//!
//! 典型用法：
//!
//! ```no_run
//! # use vireo::prelude::*;
//! # fn demo(app: &App) {
//! # let gpu = app.window_ref(&app.window(WindowDesc::new("c", 64, 64), None::<fn()>)).unwrap().gpu();
//! // 1) 用户自建 BGL（任意 group 编号，如 @group(0) @binding(0)）
//! // 2) 创建 compute 管线
//! // let kernel = app.compute_pipeline(WGSL, "cs_main", &[(0, &bgl)])?;
//! // 3) 每帧 dispatch（结果在后续 win.draw 可见）
//! // app.dispatch(&kernel, &[(0, &bind_group)], [64, 1, 1]);
//! # }
//! ```

use std::sync::Arc;

use crate::gpu::GpuContext;

/// 已编译的 GPU compute 管线句柄。
///
/// 由 [`GpuContext::create_compute_pipeline`] / `App::compute_pipeline` 创建；
/// 线程安全（`Arc` 持有）。通过 [`GpuContext::dispatch`] / `App::dispatch` 触发。
pub struct ComputePipeline {
    pipeline: wgpu::ComputePipeline,
}

impl ComputePipeline {
    /// 原始 wgpu 管线（需要自定义编码流程时的逃生口）。
    pub fn raw(&self) -> &wgpu::ComputePipeline {
        &self.pipeline
    }
}

impl GpuContext {
    /// 创建 compute 管线。
    ///
    /// - `source`：完整 WGSL（含 `@compute` 入口）。
    /// - `entry_point`：入口函数名（如 `"cs_main"`）。
    /// - `bgl_entries`：用户自带 `(group_index, BindGroupLayout)`；布局数组按最大
    ///   group 编号装配，空位为 `None`（与 P3 自管材质同规则）。传空切片 = 无绑定。
    ///
    /// 创建在 validation error scope 内执行，WGSL / 布局错误返回 `Err`（不 panic）。
    pub fn create_compute_pipeline(
        &self,
        source: &str,
        entry_point: &str,
        bgl_entries: &[(u32, &wgpu::BindGroupLayout)],
    ) -> Result<Arc<ComputePipeline>, String> {
        let scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let module = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("vireo compute shader"),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            });
        let max_group = bgl_entries.iter().map(|(gi, _)| *gi as usize).max();
        let bgls: Vec<Option<&wgpu::BindGroupLayout>> = match max_group {
            Some(max) => {
                let mut v = vec![None; max + 1];
                for (gi, bgl) in bgl_entries {
                    v[*gi as usize] = Some(*bgl);
                }
                v
            }
            None => Vec::new(),
        };
        let layout = self
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("vireo compute layout"),
                bind_group_layouts: &bgls,
                immediate_size: 0,
            });
        let pipeline = self
            .device
            .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("vireo compute pipeline"),
                layout: Some(&layout),
                module: &module,
                entry_point: Some(entry_point),
                compilation_options: Default::default(),
                cache: None,
            });
        if let Some(err) = pollster::block_on(scope.pop()) {
            return Err(format!("compute pipeline error: {err}"));
        }
        Ok(Arc::new(ComputePipeline { pipeline }))
    }

    /// 提交一次 compute dispatch。
    ///
    /// `bind_groups` 为 `(group_index, &BindGroup)`，按编号绑定（可稀疏）。
    /// 编码成独立 command buffer 并立即 `queue.submit`；wgpu 在提交边界自动插入
    /// barrier，因此后续 render pass（同一线程后续 `draw` 的提交）能读到写入结果。
    ///
    /// 任一维 workgroup 数为 0 时直接返回（无操作）。
    pub fn dispatch(
        &self,
        pipeline: &ComputePipeline,
        bind_groups: &[(u32, &wgpu::BindGroup)],
        workgroups: [u32; 3],
    ) {
        if workgroups[0] == 0 || workgroups[1] == 0 || workgroups[2] == 0 {
            return;
        }
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("vireo compute encoder"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("vireo compute pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(pipeline.raw());
            for (group, bg) in bind_groups {
                pass.set_bind_group(*group, *bg, &[]);
            }
            pass.dispatch_workgroups(workgroups[0], workgroups[1], workgroups[2]);
        }
        self.queue.submit([encoder.finish()]);
    }
}
