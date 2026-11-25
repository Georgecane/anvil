use cosmic_text::{Attrs, Buffer, FontSystem, Metrics, Shaping, SwashCache};

/// Shared font database and glyph raster cache used by native text layout/rendering.
pub struct TextSystem {
    font_system: FontSystem,
    swash_cache: SwashCache,
}
impl TextSystem {
    pub fn new() -> Self {
        Self {
            font_system: FontSystem::new(),
            swash_cache: SwashCache::new(),
        }
    }
    pub fn font_system(&self) -> &FontSystem {
        &self.font_system
    }
    pub fn font_system_mut(&mut self) -> &mut FontSystem {
        &mut self.font_system
    }
    pub fn swash_cache(&self) -> &SwashCache {
        &self.swash_cache
    }
    pub fn swash_cache_mut(&mut self) -> &mut SwashCache {
        &mut self.swash_cache
    }

    /// Shape and measure text in CSS pixels, optionally wrapping to a definite width.
    pub fn measure(
        &mut self,
        text: &str,
        max_width: Option<f32>,
        font_size: f32,
        line_height: f32,
    ) -> (f32, f32) {
        let font_size = if font_size.is_finite() && font_size > 0.0 {
            font_size
        } else {
            16.0
        };
        let line_height = if line_height.is_finite() && line_height > 0.0 {
            line_height
        } else {
            font_size * 1.2
        };
        let mut buffer = Buffer::new_empty(Metrics::new(font_size, line_height));
        buffer.set_size(max_width, None);
        buffer.set_text(text, &Attrs::new(), Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.font_system, false);

        let mut width: f32 = 0.0;
        let mut height: f32 = 0.0;
        for run in buffer.layout_runs() {
            width = width.max(run.line_w);
            height = height.max(run.line_top + run.line_height);
        }
        (width, height)
    }
}
impl Default for TextSystem {
    fn default() -> Self {
        Self::new()
    }
}
