use cosmic_text::fontdb::ID as FontFaceId;
use cosmic_text::{
    Attrs, Buffer, Family, FontSystem, Metrics, Shaping, Stretch, Style, SwashCache, Weight,
};
use std::collections::HashMap;
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

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
    font_family_faces_cache: HashMap<String, Vec<FontFaceId>>,
    font_coverage_cache: HashMap<(FontFaceId, char), bool>,
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
fn unquote_family(value: &str) -> &str {
    let value = value.trim();
    if value.len() >= 2
        && (value.starts_with('"') || value.starts_with("'"))
        && value.as_bytes().first() == value.as_bytes().last()
    {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

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
                let family = unquote_family(&value[start..index]).trim();
                if !family.is_empty() {
                    families.push(family);
                }
                start = index + character.len_utf8();
            }
            _ => {}
        }
    }

    let family = unquote_family(&value[start..]).trim();
    if !family.is_empty() {
        families.push(family);
    }
    families
}

/// CSS font fallback is resolved at grapheme boundaries so combining marks and
/// joined emoji stay together while adjacent runs using the same family remain
/// shapeable as a continuous run.
fn is_font_coverage_ignorable(character: char) -> bool {
    matches!(
        character as u32,
        0x200C..=0x200D
            | 0xFE00..=0xFE0F
            | 0xE0020..=0xE007F
            | 0xE0100..=0xE01EF
    )
}

fn select_font_family(
    grapheme: &str,
    family_count: usize,
    mut supports: impl FnMut(usize, char) -> bool,
) -> usize {
    let characters = grapheme
        .chars()
        .filter(|character| !is_font_coverage_ignorable(*character))
        .collect::<Vec<_>>();
    let mut best_family = 0;
    let mut best_coverage = 0;

    for family_index in 0..family_count {
        let coverage = characters
            .iter()
            .filter(|character| supports(family_index, **character))
            .count();

        if coverage == characters.len() {
            return family_index;
        }
        if coverage > best_coverage {
            best_coverage = coverage;
            best_family = family_index;
        }
    }

    best_family
}

fn font_family_spans(
    text: &str,
    family_count: usize,
    mut supports: impl FnMut(usize, char) -> bool,
) -> Vec<(Range<usize>, usize)> {
    if text.is_empty() || family_count == 0 {
        return Vec::new();
    }

    let mut spans = Vec::new();
    let mut active_family = None;
    let mut active_start = 0;

    for (start, grapheme) in text.grapheme_indices(true) {
        let family_index = select_font_family(grapheme, family_count, &mut supports);

        if let Some(previous_family) = active_family {
            if previous_family != family_index {
                spans.push((active_start..start, previous_family));
                active_start = start;
            }
        } else {
            active_start = start;
        }

        active_family = Some(family_index);
    }

    if let Some(family_index) = active_family {
        spans.push((active_start..text.len(), family_index));
    }

    spans
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
            font_family_faces_cache: HashMap::new(),
            font_coverage_cache: HashMap::new(),
        }
    }
    pub fn font_system(&self) -> &FontSystem {
        &self.font_system
    }
    pub fn font_system_mut(&mut self) -> &mut FontSystem {
        self.font_family_faces_cache.clear();
        self.font_coverage_cache.clear();
        &mut self.font_system
    }
    fn family_face_ids(&mut self, families: &[Family<'_>]) -> Vec<Vec<FontFaceId>> {
        families
            .iter()
            .map(|family| {
                let requested_name = self.font_system.db().family_name(family).to_owned();
                let cache_key = requested_name.to_ascii_lowercase();
                if let Some(face_ids) = self.font_family_faces_cache.get(&cache_key) {
                    return face_ids.clone();
                }

                let face_ids = self
                    .font_system
                    .db()
                    .faces()
                    .filter(|face| {
                        face.families
                            .iter()
                            .any(|(name, _)| name.eq_ignore_ascii_case(&requested_name))
                    })
                    .map(|face| face.id)
                    .collect::<Vec<_>>();
                self.font_family_faces_cache
                    .insert(cache_key, face_ids.clone());
                face_ids
            })
            .collect()
    }
    fn face_supports_character(&mut self, face_id: FontFaceId, character: char) -> bool {
        if let Some(supported) = self.font_coverage_cache.get(&(face_id, character)) {
            return *supported;
        }
        if self.font_coverage_cache.len() >= 32_768 {
            self.font_coverage_cache.clear();
        }

        let supported = self
            .font_system
            .db()
            .with_face_data(face_id, |data, face_index| {
                cosmic_text::skrifa::FontRef::from_index(data, face_index)
                    .ok()
                    .is_some_and(|font| font.charmap().map(character).is_some())
            })
            .unwrap_or(false);
        self.font_coverage_cache
            .insert((face_id, character), supported);
        supported
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
        let family_names = font_family_candidates(font.family);
        let family_names = if family_names.is_empty() {
            vec!["sans-serif"]
        } else {
            family_names
        };
        let families = family_names
            .iter()
            .map(|candidate| font_family(candidate))
            .collect::<Vec<_>>();
        let family_faces = self.family_face_ids(&families);
        let spans = font_family_spans(text, families.len(), |family_index, character| {
            family_faces[family_index]
                .iter()
                .any(|face_id| self.face_supports_character(*face_id, character))
        });

        let style = if font.style.starts_with("italic") {
            Style::Italic
        } else if font.style.starts_with("oblique") {
            Style::Oblique
        } else {
            Style::Normal
        };
        let attrs = Attrs::new()
            .family(families[0])
            .weight(Weight(font.weight.clamp(1, 1000)))
            .style(style)
            .stretch(font_stretch(font.stretch));
        let mut buffer = Buffer::new_empty(Metrics::new(font_size, line_height));
        buffer.set_size(max_width, None);
        if spans.is_empty() {
            buffer.set_text(text, &attrs, Shaping::Advanced, None);
        } else {
            let rich_spans = spans
                .iter()
                .map(|(range, family_index)| {
                    (
                        &text[range.clone()],
                        attrs.clone().family(families[*family_index]),
                    )
                })
                .collect::<Vec<_>>();
            buffer.set_rich_text(rich_spans, &attrs, Shaping::Advanced, None);
        }
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
    use super::{font_family_candidates, font_family_spans, font_stretch};
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
    fn ordered_css_fallback_is_selected_per_grapheme_and_runs_are_coalesced() {
        let spans = font_family_spans("abé", 2, |family, character| match family {
            0 => matches!(character, 'a' | 'b'),
            1 => matches!(character, 'b' | 'é'),
            _ => false,
        });

        assert_eq!(spans, vec![(0..2, 0), (2..4, 1)]);
    }

    #[test]
    fn fallback_keeps_combining_sequences_in_one_grapheme() {
        let text = "a\\u{301}b";
        let spans = font_family_spans(text, 2, |family, character| match family {
            0 => character == 'a',
            1 => character == 'a' || character == '\u{301}' || character == 'b',
            _ => false,
        });

        assert_eq!(spans, vec![(0..text.len(), 1)]);
    }

    #[test]
    fn fallback_prefers_earlier_family_when_coverage_is_equal() {
        assert_eq!(super::select_font_family("x", 2, |_, _| true), 0);
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
