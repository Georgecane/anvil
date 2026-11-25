use cosmic_text::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping, Style, SwashCache, Weight};

/// Computed font attributes passed from Stylo to native text shaping.
#[derive(Debug, Clone, Copy)]
pub struct FontAttributes<'a> {
    pub family: &'a str,
    pub weight: u16,
    pub style: &'a str,
}

/// Shared font database and glyph raster cache used by native text layout/rendering.
pub struct TextSystem {
    font_system: FontSystem,
    swash_cache: SwashCache,
}
fn first_font_family(value: &str) -> &str {
    let value = value.trim();
    let mut quote = None;
    let mut escaped = false;
    let mut end = value.len();

    for (index, character) in value.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        match (quote, character) {
            (Some(active), current) if active == current => quote = None,
            (None, '\'' | '"') => quote = Some(character),
            (None, ',') => {
                end = index;
                break;
            }
            _ => {}
        }
    }

    value[..end].trim().trim_matches(['\'', '"']).trim()
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
        font: FontAttributes<'_>,
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
        let first_family = first_font_family(font.family);
        let family = if first_family.eq_ignore_ascii_case("serif") {
            Family::Serif
        } else if first_family.eq_ignore_ascii_case("sans-serif") {
            Family::SansSerif
        } else if first_family.eq_ignore_ascii_case("monospace") {
            Family::Monospace
        } else if first_family.eq_ignore_ascii_case("cursive") {
            Family::Cursive
        } else if first_family.eq_ignore_ascii_case("fantasy") {
            Family::Fantasy
        } else {
            Family::Name(first_family)
        };
        let style = if font.style.starts_with("italic") {
            Style::Italic
        } else if font.style.starts_with("oblique") {
            Style::Oblique
        } else {
            Style::Normal
        };
        let attrs = Attrs::new()
            .family(family)
            .weight(Weight(font.weight.clamp(1, 1000)));
        let attrs = attrs.style(style);
        buffer.set_text(text, &attrs, Shaping::Advanced, None);
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
#[cfg(test)]
mod tests {
    use super::first_font_family;

    #[test]
    fn first_font_family_respects_quoted_commas_and_fallback_lists() {
        assert_eq!(
            first_font_family("\"Family, With Comma\", serif"),
            "Family, With Comma"
        );
        assert_eq!(first_font_family("Arial, sans-serif"), "Arial");
        assert_eq!(first_font_family("monospace"), "monospace");
    }
}
