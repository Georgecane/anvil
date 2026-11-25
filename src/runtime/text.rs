use cosmic_text::{
    Attrs, Buffer, Family, FontSystem, Metrics, Shaping, Stretch, Style, SwashCache, Weight,
};

/// Computed font attributes passed from Stylo to native text shaping.
#[derive(Debug, Clone, Copy)]
pub struct FontAttributes<'a> {
    pub family: &'a str,
    pub weight: u16,
    pub style: &'a str,
    pub stretch: &'a str,
}

/// Shared font database and glyph raster cache used by native text layout/rendering.
pub struct TextSystem {
    font_system: FontSystem,
    swash_cache: SwashCache,
}

fn font_family(value: &str) -> Family<'_> {
    if value.eq_ignore_ascii_case("serif") {
        Family::Serif
    } else if value.eq_ignore_ascii_case("sans-serif") {
        Family::SansSerif
    } else if value.eq_ignore_ascii_case("monospace") {
        Family::Monospace
    } else if value.eq_ignore_ascii_case("cursive") {
        Family::Cursive
    } else if value.eq_ignore_ascii_case("fantasy") {
        Family::Fantasy
    } else {
        Family::Name(value)
    }
}

/// Split a CSS font-family list without splitting commas inside quoted names.
fn font_family_candidates(value: &str) -> Vec<&str> {
    let mut families = Vec::new();
    let mut quote = None;
    let mut escaped = false;
    let mut start = 0;

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
                let family = value[start..index].trim().trim_matches(['\\'', '"']).trim();
                if !family.is_empty() {
                    families.push(family);
                }
                start = index + character.len_utf8();
            }
            _ => {}
        }
    }

    let family = value[start..].trim().trim_matches(['\\'', '"']).trim();
    if !family.is_empty() {
        families.push(family);
    }
    families
}

/// Select the first declared family available in the shared font database.
/// cosmic-text remains responsible for script-aware, per-glyph system fallback.
fn resolve_font_family<'a>(font_system: &FontSystem, value: &'a str) -> Family<'a> {
    let candidates = font_family_candidates(value);
    let first = candidates.first().copied().unwrap_or("sans-serif");

    candidates
        .iter()
        .map(|candidate| font_family(candidate))
        .find(|family| {
            let requested = font_system.db().family_name(family);
            font_system.db().faces().any(|face| {
                face.families
                    .iter()
                    .any(|(name, _)| name.eq_ignore_ascii_case(requested))
            })
        })
        .unwrap_or_else(|| font_family(first))
}

fn font_stretch(value: &str) -> Stretch {
    let value = value.trim();
    let percentage = value
        .strip_suffix('%')
        .and_then(|value| value.trim().parse::<f32>().ok());

    if let Some(percentage) = percentage.filter(|value| value.is_finite()) {
        return if percentage <= 50.0 {
            Stretch::UltraCondensed
        } else if percentage <= 62.5 {
            Stretch::ExtraCondensed
        } else if percentage <= 75.0 {
            Stretch::Condensed
        } else if percentage <= 87.5 {
            Stretch::SemiCondensed
        } else if percentage < 112.5 {
            Stretch::Normal
        } else if percentage < 125.0 {
            Stretch::SemiExpanded
        } else if percentage < 150.0 {
            Stretch::Expanded
        } else if percentage < 200.0 {
            Stretch::ExtraExpanded
        } else {
            Stretch::UltraExpanded
        };
    }

    match value.to_ascii_lowercase().as_str() {
        "ultra-condensed" => Stretch::UltraCondensed,
        "extra-condensed" => Stretch::ExtraCondensed,
        "condensed" => Stretch::Condensed,
        "semi-condensed" => Stretch::SemiCondensed,
        "semi-expanded" => Stretch::SemiExpanded,
        "expanded" => Stretch::Expanded,
        "extra-expanded" => Stretch::ExtraExpanded,
        "ultra-expanded" => Stretch::UltraExpanded,
        _ => Stretch::Normal,
    }
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
        let family = resolve_font_family(&self.font_system, font.family);
        let style = if font.style.starts_with("italic") {
            Style::Italic
        } else if font.style.starts_with("oblique") {
            Style::Oblique
        } else {
            Style::Normal
        };
        let attrs = Attrs::new()
            .family(family)
            .weight(Weight(font.weight.clamp(1, 1000)))
            .style(style)
            .stretch(font_stretch(font.stretch));
        let mut buffer = Buffer::new_empty(Metrics::new(font_size, line_height));
        buffer.set_size(max_width, None);
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
    use super::{font_family_candidates, font_stretch};
    use cosmic_text::Stretch;

    #[test]
    fn font_family_candidates_respect_quoted_commas_and_order() {
        assert_eq!(
            font_family_candidates("\"Family, With Comma\", Arial, serif"),
            vec!["Family, With Comma", "Arial", "serif"]
        );
        assert_eq!(
            font_family_candidates("'First\\' Family', \"Second, Family\", monospace"),
            vec!["First\\' Family", "Second, Family", "monospace"]
        );
        assert_eq!(font_family_candidates(" , , "), Vec::<&str>::new());
    }

    #[test]
    fn font_stretch_maps_css_percentage_steps() {
        assert_eq!(font_stretch("50%"), Stretch::UltraCondensed);
        assert_eq!(font_stretch("75%"), Stretch::Condensed);
        assert_eq!(font_stretch("87.5%"), Stretch::SemiCondensed);
        assert_eq!(font_stretch("100%"), Stretch::Normal);
        assert_eq!(font_stretch("125%"), Stretch::Expanded);
        assert_eq!(font_stretch("200%"), Stretch::UltraExpanded);
        assert_eq!(font_stretch("semi-expanded"), Stretch::SemiExpanded);
        assert_eq!(font_stretch("invalid"), Stretch::Normal);
    }
}
