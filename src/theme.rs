use crate::models::ThemeMode;
use egui::{Color32, CornerRadius, Stroke, Style, Visuals};

#[derive(Debug, Clone, Copy)]
pub struct ThemePalette {
    pub is_dark: bool,
    pub bg_root: Color32,
    pub bg_card: Color32,
    pub bg_subcard: Color32,
    pub bg_input: Color32,
    pub bg_badge: Color32,
    pub bg_console: Color32,
    pub border: Color32,
    pub border_input: Color32,
    pub accent: Color32,
    pub text_primary: Color32,
    pub text_secondary: Color32,
    pub text_accent: Color32,
    pub btn_secondary: Color32,
    pub btn_secondary_hover: Color32,
    pub btn_danger: Color32,
    pub btn_success: Color32,
}

impl ThemePalette {
    pub fn dark() -> Self {
        Self {
            is_dark: true,
            bg_root: Color32::from_rgb(0x16, 0x18, 0x1d),
            bg_card: Color32::from_rgb(0x20, 0x24, 0x2b),
            bg_subcard: Color32::from_rgb(0x1a, 0x1d, 0x23),
            bg_input: Color32::from_rgb(0x14, 0x17, 0x1c),
            bg_badge: Color32::from_rgb(0x1b, 0x22, 0x2c),
            bg_console: Color32::from_rgb(0x12, 0x14, 0x18),
            border: Color32::from_rgb(0x2e, 0x34, 0x40),
            border_input: Color32::from_rgb(0x37, 0x3e, 0x4d),
            accent: Color32::from_rgb(0x1f, 0x6a, 0xa5),
            text_primary: Color32::from_rgb(0xe6, 0xed, 0xf3),
            text_secondary: Color32::from_rgb(0x9b, 0xa3, 0xae),
            text_accent: Color32::from_rgb(0x5d, 0xad, 0xe2),
            btn_secondary: Color32::from_rgb(0x2d, 0x33, 0x3b),
            btn_secondary_hover: Color32::from_rgb(0x37, 0x3e, 0x47),
            btn_danger: Color32::from_rgb(0xb8, 0x32, 0x32),
            btn_success: Color32::from_rgb(0x27, 0xae, 0x60),
        }
    }

    pub fn light() -> Self {
        Self {
            is_dark: false,
            bg_root: Color32::from_rgb(0xf0, 0xf2, 0xf5),
            bg_card: Color32::from_rgb(0xff, 0xff, 0xff),
            bg_subcard: Color32::from_rgb(0xf6, 0xf8, 0xfa),
            bg_input: Color32::from_rgb(0xff, 0xff, 0xff),
            bg_badge: Color32::from_rgb(0xe6, 0xeb, 0xf2),
            bg_console: Color32::from_rgb(0xf8, 0xfa, 0xfc),
            border: Color32::from_rgb(0xd0, 0xd7, 0xde),
            border_input: Color32::from_rgb(0xaf, 0xb8, 0xc1),
            accent: Color32::from_rgb(0x1f, 0x6a, 0xa5),
            text_primary: Color32::from_rgb(0x1f, 0x23, 0x28),
            text_secondary: Color32::from_rgb(0x57, 0x60, 0x6a),
            text_accent: Color32::from_rgb(0x1f, 0x6a, 0xa5),
            btn_secondary: Color32::from_rgb(0x3b, 0x8e, 0xd0),
            btn_secondary_hover: Color32::from_rgb(0x36, 0x71, 0x9f),
            btn_danger: Color32::from_rgb(0xc9, 0x37, 0x37),
            btn_success: Color32::from_rgb(0x1e, 0x82, 0x4c),
        }
    }
}

#[cfg(target_os = "windows")]
pub fn detect_windows_light_mode() -> bool {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    if let Ok(key) =
        hkcu.open_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize")
    {
        if let Ok(val) = key.get_value::<u32, _>("AppsUseLightTheme") {
            return val == 1;
        }
    }
    false
}

#[cfg(not(target_os = "windows"))]
pub fn detect_windows_light_mode() -> bool {
    false
}

pub fn apply_theme(ctx: &egui::Context, mode: ThemeMode) -> ThemePalette {
    let use_light = match mode {
        ThemeMode::Dark => false,
        ThemeMode::Light => true,
        ThemeMode::System => detect_windows_light_mode(),
    };
    let palette = if use_light {
        ThemePalette::light()
    } else {
        ThemePalette::dark()
    };

    let mut visuals = if palette.is_dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };

    visuals.panel_fill = palette.bg_root;
    visuals.window_fill = palette.bg_card;
    visuals.extreme_bg_color = palette.bg_input;
    visuals.faint_bg_color = palette.bg_subcard;
    visuals.selection.bg_fill = palette.accent;
    visuals.selection.stroke = Stroke::new(1.0_f32, Color32::WHITE);

    visuals.widgets.noninteractive.bg_fill = palette.bg_card;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, palette.border);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, palette.text_primary);
    visuals.widgets.noninteractive.corner_radius = CornerRadius::same(6);

    visuals.widgets.inactive.bg_fill = if palette.is_dark {
        palette.btn_secondary
    } else {
        palette.bg_badge
    };
    visuals.widgets.inactive.weak_bg_fill = palette.bg_input;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, palette.border_input);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, palette.text_primary);
    visuals.widgets.inactive.corner_radius = CornerRadius::same(5);

    visuals.widgets.hovered.bg_fill = palette.btn_secondary_hover;
    visuals.widgets.hovered.weak_bg_fill = palette.btn_secondary_hover;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, palette.text_accent);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, Color32::WHITE);
    visuals.widgets.hovered.corner_radius = CornerRadius::same(5);

    visuals.widgets.active.bg_fill = palette.accent;
    visuals.widgets.active.weak_bg_fill = palette.accent;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0_f32, palette.text_accent);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0_f32, Color32::WHITE);
    visuals.widgets.active.corner_radius = CornerRadius::same(5);

    let mut style = Style {
        visuals,
        ..Default::default()
    };
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(10.0, 5.0);
    ctx.set_style(style);

    palette
}

#[cfg(test)]
mod tests {
    use super::*;

    fn relative_luminance(c: Color32) -> f32 {
        0.2126 * (c.r() as f32 / 255.0)
            + 0.7152 * (c.g() as f32 / 255.0)
            + 0.0722 * (c.b() as f32 / 255.0)
    }

    #[test]
    fn test_theme_palettes_high_contrast() {
        for palette in [ThemePalette::light(), ThemePalette::dark()] {
            let text_lum = relative_luminance(palette.text_primary);
            let input_lum = relative_luminance(palette.bg_input);
            let card_lum = relative_luminance(palette.bg_card);
            assert!((text_lum - input_lum).abs() > 0.5);
            assert!((text_lum - card_lum).abs() > 0.5);
        }
    }
}
