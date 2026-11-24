use cosmic_text::{FontSystem, SwashCache};

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
}
impl Default for TextSystem {
    fn default() -> Self {
        Self::new()
    }
}
