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
