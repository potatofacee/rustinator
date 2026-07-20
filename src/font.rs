use crossfont::{
    FontDesc, FontKey, GlyphKey, Metrics, Rasterize, RasterizedGlyph, Rasterizer, Size, Slant,
    Style, Weight,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FontStyle {
    Regular,
    Bold,
    Italic,
    BoldItalic,
}

impl FontStyle {
    pub const ALL: [FontStyle; 4] = [
        FontStyle::Regular,
        FontStyle::Bold,
        FontStyle::Italic,
        FontStyle::BoldItalic,
    ];

    fn slant_weight(self) -> (Slant, Weight) {
        match self {
            FontStyle::Regular => (Slant::Normal, Weight::Normal),
            FontStyle::Bold => (Slant::Normal, Weight::Bold),
            FontStyle::Italic => (Slant::Italic, Weight::Normal),
            FontStyle::BoldItalic => (Slant::Italic, Weight::Bold),
        }
    }

    fn index(self) -> usize {
        match self {
            FontStyle::Regular => 0,
            FontStyle::Bold => 1,
            FontStyle::Italic => 2,
            FontStyle::BoldItalic => 3,
        }
    }
}

pub struct FontContext {
    rasterizer: Rasterizer,
    /// Regular / Bold / Italic / BoldItalic — indexed by FontStyle::index.
    keys: [FontKey; 4],
    size: Size,
    pub metrics: Metrics,
}

impl FontContext {
    pub fn new(family: &str, size_pt: f32) -> Result<Self, crossfont::Error> {
        let mut rasterizer = Rasterizer::new()?;
        let size = Size::new(size_pt);

        // Load regular first; fall back to regular for any style whose face
        // fontconfig can't find.
        let regular_desc = FontDesc::new(
            family,
            Style::Description {
                slant: Slant::Normal,
                weight: Weight::Normal,
            },
        );
        let regular_key = rasterizer.load_font(&regular_desc, size)?;

        let mut keys = [regular_key; 4];
        for style in FontStyle::ALL {
            if style == FontStyle::Regular {
                continue;
            }
            let (slant, weight) = style.slant_weight();
            let desc = FontDesc::new(family, Style::Description { slant, weight });
            keys[style.index()] = rasterizer.load_font(&desc, size).unwrap_or(regular_key);
        }

        // Prime regular to force set_char_size so metrics() returns real data.
        let _ = rasterizer.get_glyph(GlyphKey {
            character: '0',
            font_key: regular_key,
            size,
        });
        let metrics = rasterizer.metrics(regular_key, size)?;

        Ok(Self {
            rasterizer,
            keys,
            size,
            metrics,
        })
    }

    pub fn cell_width(&self) -> f32 {
        self.metrics.average_advance as f32
    }

    pub fn cell_height(&self) -> f32 {
        self.metrics.line_height as f32
    }

    pub fn rasterize(
        &mut self,
        c: char,
        style: FontStyle,
    ) -> Result<RasterizedGlyph, crossfont::Error> {
        self.rasterizer.get_glyph(GlyphKey {
            character: c,
            font_key: self.keys[style.index()],
            size: self.size,
        })
    }
}

/// Font multiplier for scaled-zoom: how much bigger the same content can be
/// drawn when a pane of `pane_w` x `pane_h` is maximized to `full_w` x `full_h`.
/// Returns `min(full_w/pane_w, full_h/pane_h)` clamped to `>= 1.0`, and `1.0`
/// if any dimension is non-positive. Free-standing (takes f32s, not egui types).
pub fn scaled_zoom_factor(pane_w: f32, pane_h: f32, full_w: f32, full_h: f32) -> f32 {
    if pane_w <= 0.0 || pane_h <= 0.0 || full_w <= 0.0 || full_h <= 0.0 {
        return 1.0;
    }
    (full_w / pane_w).min(full_h / pane_h).max(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaled_zoom_factor_half_width_pane() {
        // Half width but full height: the height axis is already maxed, so the
        // tighter axis (height, 1.0) wins -- no scale, else it would overflow.
        assert_eq!(scaled_zoom_factor(50.0, 100.0, 100.0, 100.0), 1.0);
    }

    #[test]
    fn scaled_zoom_factor_equal_rects_is_one() {
        assert_eq!(scaled_zoom_factor(100.0, 100.0, 100.0, 100.0), 1.0);
    }

    #[test]
    fn scaled_zoom_factor_uses_min_axis() {
        // Width allows 4x, height allows 2x -> the tighter axis wins.
        assert_eq!(scaled_zoom_factor(25.0, 50.0, 100.0, 100.0), 2.0);
    }

    #[test]
    fn scaled_zoom_factor_never_below_one() {
        // Pane larger than the window must not shrink the font.
        assert_eq!(scaled_zoom_factor(200.0, 200.0, 100.0, 100.0), 1.0);
    }

    #[test]
    fn scaled_zoom_factor_zero_or_negative_dim_is_one() {
        assert_eq!(scaled_zoom_factor(0.0, 100.0, 100.0, 100.0), 1.0);
        assert_eq!(scaled_zoom_factor(100.0, 0.0, 100.0, 100.0), 1.0);
        assert_eq!(scaled_zoom_factor(100.0, 100.0, 0.0, 100.0), 1.0);
        assert_eq!(scaled_zoom_factor(100.0, 100.0, 100.0, -5.0), 1.0);
    }

    #[test]
    fn font_style_index_matches_all_ordering() {
        for (expected, style) in FontStyle::ALL.iter().enumerate() {
            assert_eq!(
                style.index(),
                expected,
                "FontStyle::{:?} index {} does not match its position {} in FontStyle::ALL",
                style,
                style.index(),
                expected,
            );
        }
    }
}
