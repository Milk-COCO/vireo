use wgpu::{DepthStencilState, StencilFaceState, StencilState};

/// 文字管线与 render pass DS attachment 的匹配方式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TextStencilMode {
    /// pass 无 DS attachment
    None,
    /// pass 有 DS，文字不测模板（Always；用于 UI / unclipped）
    Pass,
    /// pass 有 DS，Equal+Keep 测模板（裁切区内文字）
    Test,
}

/// Equal+Keep：只画在当前 stencil ref 内，不写 stencil。
pub(crate) fn stencil_text_ds_test() -> Option<DepthStencilState> {
    Some(DepthStencilState {
        format: wgpu::TextureFormat::Depth24PlusStencil8,
        depth_write_enabled: Some(false),
        depth_compare: Some(wgpu::CompareFunction::Always),
        stencil: StencilState {
            front: StencilFaceState {
                compare: wgpu::CompareFunction::Equal,
                fail_op: wgpu::StencilOperation::Keep,
                depth_fail_op: wgpu::StencilOperation::Keep,
                pass_op: wgpu::StencilOperation::Keep,
            },
            back: StencilFaceState {
                compare: wgpu::CompareFunction::Equal,
                fail_op: wgpu::StencilOperation::Keep,
                depth_fail_op: wgpu::StencilOperation::Keep,
                pass_op: wgpu::StencilOperation::Keep,
            },
            read_mask: 0xff,
            write_mask: 0x00,
        },
        bias: wgpu::DepthBiasState::default(),
    })
}

/// Always：有 DS attachment 时透传（不测不写），避免 UI/unclipped 被 Equal(0) 误裁。
pub(crate) fn stencil_text_ds_pass() -> Option<DepthStencilState> {
    Some(DepthStencilState {
        format: wgpu::TextureFormat::Depth24PlusStencil8,
        depth_write_enabled: Some(false),
        depth_compare: Some(wgpu::CompareFunction::Always),
        stencil: StencilState {
            front: StencilFaceState::IGNORE,
            back: StencilFaceState::IGNORE,
            read_mask: 0,
            write_mask: 0,
        },
        bias: wgpu::DepthBiasState::default(),
    })
}
