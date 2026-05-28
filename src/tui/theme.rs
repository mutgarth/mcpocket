use ratatui::style::Color;

/// Brand palette derived from the mcpocket / Jules design system
/// (deep purple `#1D0245` .. light `#B898E8`).
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub bg: Color,
    pub fg: Color,
    pub accent: Color,
    pub selection: Color,
    pub dim: Color,
    pub ok: Color,
    pub warn: Color,
    pub fail: Color,
}

impl Theme {
    /// Build the theme. `truecolor` selects 24-bit RGB; otherwise indexed ANSI.
    pub fn brand(truecolor: bool) -> Self {
        if truecolor {
            Self {
                bg: Color::Rgb(0x15, 0x0F, 0x36),
                fg: Color::Rgb(0xED, 0xE0, 0xFA),
                accent: Color::Rgb(0x9C, 0x70, 0xE0),
                selection: Color::Rgb(0x7B, 0x4E, 0xD8),
                dim: Color::Rgb(0xB8, 0x98, 0xE8),
                ok: Color::Rgb(0x6E, 0xE7, 0xB7),
                warn: Color::Rgb(0xFB, 0xBF, 0x24),
                fail: Color::Rgb(0xF8, 0x71, 0x71),
            }
        } else {
            Self {
                bg: Color::Reset,
                fg: Color::White,
                accent: Color::Magenta,
                selection: Color::LightMagenta,
                dim: Color::Gray,
                ok: Color::Green,
                warn: Color::Yellow,
                fail: Color::Red,
            }
        }
    }

    /// Detect terminal truecolor support via `COLORTERM`.
    pub fn detect() -> Self {
        let truecolor = std::env::var("COLORTERM")
            .map(|v| v.contains("truecolor") || v.contains("24bit"))
            .unwrap_or(false);
        Self::brand(truecolor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    #[test]
    fn truecolor_theme_uses_rgb_accent() {
        let theme = Theme::brand(true);
        assert!(matches!(theme.accent, Color::Rgb(_, _, _)));
    }

    #[test]
    fn fallback_theme_uses_indexed_accent() {
        let theme = Theme::brand(false);
        assert!(matches!(theme.accent, Color::Magenta | Color::LightMagenta));
    }

    #[test]
    fn status_colors_distinguish_ok_and_fail() {
        let theme = Theme::brand(true);
        assert_ne!(theme.ok, theme.fail);
    }
}
