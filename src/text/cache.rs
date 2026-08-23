use std::sync::Arc;
use std::time::Instant;

use cosmic_text::{AttrsOwned, Buffer};

use crate::text::{TextAlign, TextDef};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ShapeKey {
    pub(crate) text: String,
    font_size_bits: u32,
    max_width_bits: u32,
    align: u8,
    attrs: Option<AttrsOwned>,
}

impl ShapeKey {
    pub(crate) fn from_text(text: &str, options: &TextDef) -> Self {
        let max_width_bits = options.max_width.map(|w| w.to_bits()).unwrap_or(u32::MAX);
        Self {
            text: text.to_string(),
            font_size_bits: options.font_size.to_bits(),
            max_width_bits,
            align: options.align as u8,
            attrs: options.attrs.clone(),
        }
    }
}

/// 单字符缓存键：栈上分配，命中 0 分配 / 1 哈希，未命中 2 分配 / 2 哈希。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct GlyphKey {
    pub(crate) ch: char,
    font_size_bits: u32,
    max_width_bits: u32,
    align: u8,
    attrs: Option<AttrsOwned>,
}

impl GlyphKey {
    pub(crate) fn from_char(ch: char, options: &TextDef) -> Self {
        Self {
            ch,
            font_size_bits: options.font_size.to_bits(),
            max_width_bits: u32::MAX,
            align: TextAlign::Left as u8,
            attrs: options.attrs.clone(),
        }
    }
}

pub(crate) struct ShapeCacheSlot {
    pub(crate) key: ShapeKey,
    pub(crate) buffer: Arc<Buffer>,
    pub(crate) line_width: f32,
    pub(crate) liveness: Option<Arc<()>>,
    pub(crate) last_used: Instant,
}

#[derive(Default, Debug, Clone, Copy)]
pub struct ShapeCacheStats {
    pub hits: u64,
    pub misses: u64,
    pub gc_runs: u64,
    pub last_gc_us: u64,
    pub total_gc_us: u64,
}
