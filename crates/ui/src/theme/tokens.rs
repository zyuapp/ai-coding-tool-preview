//! The semantic tokens of styles/tokens.css, derived from a palette exactly as the stylesheet does.
//! A theme only changes the palette; nothing here is picked per theme.

use gpui::{point, px, BoxShadow, Rgba};

use super::palette::Palette;
use super::ThemeVariant;
use crate::color::{alpha, hex, mix, over, rgb_triple, TRANSPARENT, WHITE};

/// What the platform draws under the window, which the frame and the page thin to show.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WindowMaterial {
    /// An opaque window in the theme's canvas.
    #[default]
    None,
    /// macOS vibrancy under the frame (`data-window-material`).
    Vibrancy,
    /// Liquid Glass: the window is one sheet (`data-window-material` and `data-glass-sheet`).
    Glass,
}

/// The parts of the environment the tokens answer to, beside the palette.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Environment {
    pub material: WindowMaterial,
    /// `prefers-reduced-transparency: reduce`.
    pub reduced_transparency: bool,
    /// `prefers-reduced-motion: reduce`.
    pub reduced_motion: bool,
    /// How thick the glass is, 0 to 1. Over the system's own glass it is the tint set there.
    pub glass_tint: f32,
    /// What the platform's material reads as under the window on a dark and a light ground. Given,
    /// the frame is composited onto it, so an opaque window draws what a translucent one would show
    /// over that material.
    pub material_ground: Option<MaterialGround>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MaterialGround {
    pub dark: Rgba,
    pub light: Rgba,
}

impl Default for Environment {
    fn default() -> Self {
        Self { material: WindowMaterial::None, reduced_transparency: false, reduced_motion: false, glass_tint: 0.6, material_ground: None }
    }
}

impl Environment {
    /// macOS Liquid Glass at the system's default tint (0), with the window drawn opaque over the
    /// grey the system glass reads as on each ground.
    pub fn macos_glass() -> Self {
        let ground = MaterialGround { dark: hex(0x616464), light: hex(0xf2f2f2) };
        Self { material: WindowMaterial::Glass, glass_tint: 0.0, material_ground: Some(ground), ..Self::default() }
    }

    fn material_shows(&self) -> bool {
        self.material != WindowMaterial::None && !self.reduced_transparency
    }

    fn glass_sheet(&self) -> bool {
        self.material == WindowMaterial::Glass && !self.reduced_transparency
    }
}

/// One fill, optionally under the glass sheen: a gradient of white from 8% at the top to 2% at the
/// bottom, painted over the colour.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pane {
    pub fill: Rgba,
    pub sheen: bool,
}

impl Pane {
    pub const fn solid(fill: Rgba) -> Self {
        Self { fill, sheen: false }
    }

    pub const fn sheened(fill: Rgba) -> Self {
        Self { fill, sheen: true }
    }

    pub fn background(&self) -> gpui::Background {
        if !self.sheen {
            return gpui::solid_background(self.fill);
        }
        let top = over(alpha(WHITE, 0.08), self.fill);
        let bottom = over(alpha(WHITE, 0.02), self.fill);
        gpui::linear_gradient(180.0, gpui::linear_color_stop(top, 0.0), gpui::linear_color_stop(bottom, 1.0))
    }

    /// The colour the pane reads as at its middle, for anything that needs one flat colour.
    pub fn flat(&self) -> Rgba {
        if self.sheen {
            over(alpha(WHITE, 0.05), self.fill)
        } else {
            self.fill
        }
    }
}

/// The light caught along a raised thing's edges, drawn inside it: a hairline all round, brighter
/// along the top and faint along the bottom.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rim {
    pub hairline: Rgba,
    pub top: Rgba,
    pub bottom: Rgba,
}

/// One outer shadow layer, in CSS terms.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShadowLayer {
    pub x: f32,
    pub y: f32,
    pub blur: f32,
    pub spread: f32,
    pub color: Rgba,
}

impl ShadowLayer {
    pub fn box_shadow(&self) -> BoxShadow {
        BoxShadow {
            color: self.color.into(),
            offset: point(px(self.x), px(self.y)),
            blur_radius: px(self.blur),
            spread_radius: px(self.spread),
        }
    }
}

/// A raised thing's shadow: the top-edge glint (`--glint`), drawn inside, and the outer layers.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Elevation {
    pub glint: bool,
    pub layers: Vec<ShadowLayer>,
}

impl Elevation {
    pub fn box_shadows(&self) -> Vec<BoxShadow> {
        self.layers.iter().map(ShadowLayer::box_shadow).collect()
    }
}

/// A timing curve and duration (`--*-time` and `--*-ease`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Motion {
    pub millis: u64,
    /// The cubic-bezier's control points.
    pub ease: [f32; 4],
}

impl Motion {
    pub fn duration(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.millis)
    }

    /// The eased progress at `t` in 0–1.
    pub fn ease(&self, t: f32) -> f32 {
        cubic_bezier(self.ease, t)
    }
}

/// Solves a CSS cubic-bezier for its y at x = `t`.
pub fn cubic_bezier([x1, y1, x2, y2]: [f32; 4], t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let sample = |a: f32, b: f32, s: f32| {
        let inv = 1.0 - s;
        3.0 * inv * inv * s * a + 3.0 * inv * s * s * b + s * s * s
    };
    let mut lo = 0.0f32;
    let mut hi = 1.0f32;
    let mut s = t;
    for _ in 0..24 {
        let x = sample(x1, x2, s);
        if (x - t).abs() < 1e-5 {
            break;
        }
        if x < t {
            lo = s;
        } else {
            hi = s;
        }
        s = (lo + hi) / 2.0;
    }
    sample(y1, y2, s)
}

/// A state's hue as the six roles a toned primitive reads (`[data-tone]` in tone.css).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ToneColors {
    /// The hue as a fill.
    pub tone: Rgba,
    /// As text or an icon.
    pub text: Rgba,
    /// As quieter text.
    pub soft: Rgba,
    /// As an edge.
    pub line: Rgba,
    /// A pale wash.
    pub surface: Rgba,
    /// A stronger wash.
    pub chip: Rgba,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tone {
    Accent,
    Danger,
    Info,
    Success,
    Warning,
}

/// Every token in tokens.css, resolved for one palette and environment.
#[derive(Clone, Debug, PartialEq)]
pub struct Tokens {
    pub variant: ThemeVariant,

    // Surfaces
    pub canvas: Rgba,
    pub canvas_fade: Rgba,
    pub sunken: Rgba,
    /// `--sunken` inside the dock and the inspector, which on the glass sheet have no ground of their own.
    pub dock_sunken: Rgba,
    pub sidebar: Rgba,
    pub surface: Rgba,
    pub surface_raised: Rgba,
    pub surface_menu: Rgba,
    pub surface_menu_hover: Rgba,
    pub surface_hover: Rgba,
    pub surface_active: Rgba,
    pub row_hover: Rgba,
    pub row_selected: Rgba,
    pub surface_input: Rgba,
    pub surface_glass: Rgba,
    pub frame: Rgba,
    /// The frame before it is composited onto `ground`, for the window's own background.
    pub frame_clear: Rgba,
    /// What the window's material reads as, when the frame is composited onto it.
    pub ground: Option<Rgba>,
    pub page: Rgba,
    pub frame_control: Rgba,
    pub frame_control_hover: Rgba,
    pub frame_row_hover: Rgba,
    pub frame_row_selected: Rgba,
    pub frame_field: Rgba,
    pub frame_meta: Rgba,
    pub frame_label: Rgba,
    pub surface_inverse: Rgba,
    pub surface_inverse_soft: Rgba,
    pub surface_inverse_off: Rgba,
    pub control: Rgba,
    pub control_soft: Rgba,
    pub control_hover: Rgba,
    pub chip: Rgba,
    pub wash: Rgba,
    pub code_surface: Rgba,
    pub code_ink: Rgba,
    pub meter_fill: Rgba,
    pub meter_track: Rgba,

    // Lines
    pub line: Rgba,
    pub line_soft: Rgba,
    pub line_mid: Rgba,
    pub line_strong: Rgba,
    pub line_bright: Rgba,
    pub line_glass: Rgba,
    pub line_input: Rgba,
    pub line_control: Rgba,
    pub scroll_thumb: Rgba,

    // Text
    pub ink: Rgba,
    pub ink_soft: Rgba,
    pub ink_dim: Rgba,
    pub ink_faint: Rgba,
    pub muted_strong: Rgba,
    pub muted: Rgba,
    pub muted_soft: Rgba,
    pub quiet_strong: Rgba,
    pub quiet: Rgba,
    pub quiet_soft: Rgba,
    pub disabled_opacity: f32,
    pub ink_inverse: Rgba,
    pub ink_inverse_soft: Rgba,

    // Accent
    pub accent: Rgba,
    pub accent_text: Rgba,
    pub accent_hover: Rgba,
    pub accent_bright: Rgba,
    pub accent_soft: Rgba,
    pub accent_ink: Rgba,
    pub accent_surface: Rgba,
    pub accent_chip: Rgba,
    pub accent_line: Rgba,
    pub accent_line_strong: Rgba,
    pub accent_line_soft: Rgba,

    // Info
    pub info: Rgba,
    pub info_soft: Rgba,
    pub info_surface: Rgba,
    pub info_surface_soft: Rgba,
    pub info_line: Rgba,
    pub info_badge: Rgba,
    pub info_ink: Rgba,
    pub badge_ink: Rgba,
    pub focus_ring: Rgba,
    pub link: Rgba,
    pub link_hover: Rgba,
    pub link_line: Rgba,
    /// Selected text, in the platform's default highlight.
    pub text_selection: Rgba,

    /// The six colours a checkout's mark takes.
    pub worktree: [Rgba; 6],
    pub role_coordinator: Rgba,
    pub role_implementer: Rgba,
    pub role_reviewer: Rgba,
    pub role_researcher: Rgba,

    // Success
    pub success: Rgba,
    pub success_soft: Rgba,
    pub success_surface: Rgba,
    pub success_line: Rgba,
    pub success_ink: Rgba,

    // Danger
    pub danger: Rgba,
    pub danger_ink: Rgba,
    pub danger_soft: Rgba,
    pub danger_surface: Rgba,
    pub danger_chip: Rgba,
    pub danger_line: Rgba,
    pub danger_line_hover: Rgba,

    pub warning: Rgba,
    pub pr_merged: Rgba,

    pub alert: Rgba,
    pub alert_hover: Rgba,
    pub alert_line: Rgba,
    pub alert_ink: Rgba,

    // Terminal
    pub terminal_selection: Rgba,
    pub find_match: Rgba,
    pub find_active: Rgba,
    /// black, red, green, yellow, blue, magenta, cyan, white, then the bright eight.
    pub ansi: [Rgba; 16],

    // Diff
    pub diff_add_surface: Rgba,
    pub diff_add_line: Rgba,
    pub diff_add_gutter: Rgba,
    pub diff_del_surface: Rgba,
    pub diff_del_line: Rgba,
    pub diff_del_gutter: Rgba,
    pub diff_hunk_surface: Rgba,
    pub diff_hunk_ink: Rgba,
    pub diff_gutter_ink: Rgba,
    pub diff_absent: Rgba,
    pub diff_selected: Rgba,
    pub diff_selected_line: Rgba,

    // Syntax
    pub syntax_comment: Rgba,
    pub syntax_string: Rgba,
    pub syntax_number: Rgba,
    pub syntax_constant: Rgba,
    pub syntax_keyword: Rgba,
    pub syntax_operator: Rgba,
    pub syntax_function: Rgba,
    pub syntax_type: Rgba,
    pub syntax_variable: Rgba,
    pub syntax_tag: Rgba,
    pub syntax_attribute: Rgba,
    pub syntax_punctuation: Rgba,

    // Glass
    /// `--glint`: the light along a raised thing's top edge.
    pub glint: Rgba,
    pub glass_tint: f32,
    pub glass: Pane,
    pub glass_menu: Pane,
    pub glass_clear: Pane,
    /// Whether the glass panes blur what is under them.
    pub glass_blur: bool,
    pub glass_strip: Rgba,
    /// What a toned glass surface mixes its hue into.
    pub tone_glass_ground: Rgba,
    pub glass_rim: Rim,
    /// The float rim's dark hairline outside the edge.
    pub glass_float_outline: Rgba,
    /// The 1px edge a float's border catches light along.
    pub glass_edge: Rgba,
    pub glass_control: Pane,
    pub glass_control_hover: Pane,
    pub glass_pick: Pane,
    pub glass_pick_danger: Pane,

    // Shadows and scrims
    pub shadow_menu: Elevation,
    pub shadow_popover: Elevation,
    pub shadow_panel: Elevation,
    pub shadow_composer: Elevation,
    pub shadow_float: Elevation,
    pub shadow_float_lift: Elevation,
    pub shadow_overlay: Elevation,
    pub shadow_page: Elevation,
    pub shadow_sidebar: Elevation,
    pub shadow_thumb: Elevation,
    pub scrim: Rgba,
    pub veil: Rgba,
    pub veil_deep: Rgba,
    pub veil_blur: bool,
    pub scrim_soft: Rgba,

    // Motion
    pub panel_slide: Motion,
    pub glide: Motion,
    pub touch: Motion,
    pub reduced_motion: bool,
}

const SLIDE_EASE: [f32; 4] = [0.32, 0.72, 0.0, 1.0];
const TOUCH_EASE: [f32; 4] = [0.2, 0.0, 0.0, 1.0];

impl Tokens {
    pub fn derive(p: &Palette, env: &Environment) -> Self {
        let c = hex;
        let dark = p.variant == ThemeVariant::Dark;
        let light_dark = |light: f32, dark_value: f32| if dark { dark_value } else { light };
        let tint = env.glass_tint.clamp(0.0, 1.0);

        let bg = [c(p.bg_0), c(p.bg_1), c(p.bg_2), c(p.bg_3), c(p.bg_4), c(p.bg_5), c(p.bg_6)];
        let line = [c(p.line_0), c(p.line_1), c(p.line_2), c(p.line_3)];
        let fg = [c(p.fg_0), c(p.fg_1), c(p.fg_2), c(p.fg_3), c(p.fg_4), c(p.fg_5)];
        let fg0 = fg[0];
        let shade = |weight: f32| rgb_triple(p.shadow, weight * p.shadow_scale);
        let ink_wash = |a: f32| alpha(fg0, a);

        let canvas = bg[0];
        let mut sunken = bg[1];
        let sidebar = bg[3];
        let mut surface = bg[2];
        let mut surface_raised = bg[4];
        let mut surface_menu = bg[3];
        let mut surface_menu_hover = bg[6];
        let mut surface_hover = bg[4];
        let mut surface_active = bg[5];
        let mut row_hover = p.row_hover.map(c).unwrap_or(bg[6]);
        let mut row_selected = p.row_selected.map(c).unwrap_or_else(|| mix(bg[6], 0.93, fg0));
        let mut surface_input = mix(bg[0], 0.5, bg[2]);
        let surface_glass = alpha(bg[4], 0.95);
        let mut control = bg[5];
        let mut control_soft = bg[3];
        let mut control_hover = mix(bg[5], 0.4, bg[6]);
        let mut chip = mix(bg[6], 0.6, bg[5]);
        let wash = alpha(fg0, p.wash_alpha);
        let mut code_surface = bg[4];
        let mut meter_track = mix(bg[5], 0.6, bg[6]);

        let mut l_line = line[1];
        let mut line_soft = line[0];
        let mut line_mid = mix(line[1], 0.5, line[2]);
        let mut line_strong = line[3];
        let mut line_bright = mix(line[3], 0.65, fg[4]);
        let line_glass = alpha(fg0, 0.11);
        let line_input = mix(fg[4], 0.75, line[2]);
        let mut line_control = mix(line[2], 0.5, line[3]);

        let quiet_strong = fg[3];
        let quiet = fg[4];
        let quiet_soft = mix(fg[4], 0.75, fg[5]);

        let accent = c(p.accent);
        let accent_text = p.accent_text.map(c).unwrap_or(accent);
        let info_hue = c(p.info);
        let info = p.info_text.map(c).unwrap_or(info_hue);
        let link = p.link_text.map(c).unwrap_or(c(p.link));
        let on_hue = c(p.on_hue);
        let success_hue = c(p.success);
        let success = p.success_text.map(c).unwrap_or(success_hue);
        let danger_hue = c(p.danger);
        let danger = p.danger_text.map(c).unwrap_or(danger_hue);
        let warning = p.warning_text.map(c).unwrap_or(c(p.warning));
        let alert = c(p.alert);

        let mut accent_surface = mix(accent, 0.09, bg[0]);
        let mut accent_chip = mix(accent, 0.17, bg[1]);
        let mut accent_line = mix(accent, 0.32, bg[5]);
        let mut info_surface = mix(info_hue, 0.16, bg[1]);
        let mut info_surface_soft = mix(info_hue, 0.09, bg[1]);
        let mut info_line = mix(info_hue, 0.28, bg[5]);
        let mut success_surface = mix(success_hue, 0.18, bg[1]);
        let mut danger_surface = mix(danger_hue, 0.10, bg[1]);
        let mut danger_chip = mix(danger_hue, 0.18, bg[1]);
        let mut danger_line = mix(danger_hue, 0.32, bg[5]);

        let mut diff_add_surface = mix(success_hue, 0.12, bg[0]);
        let mut diff_add_line = mix(success_hue, 0.30, bg[1]);
        let mut diff_add_gutter = mix(success_hue, 0.18, bg[1]);
        let mut diff_del_surface = mix(danger_hue, 0.12, bg[0]);
        let mut diff_del_line = mix(danger_hue, 0.30, bg[1]);
        let mut diff_del_gutter = mix(danger_hue, 0.18, bg[1]);
        let mut diff_hunk_surface = mix(fg0, 0.03, bg[1]);
        let mut diff_absent = mix(bg[1], 0.8, bg[2]);
        let mut diff_selected = mix(info_hue, 0.15, bg[3]);
        let mut diff_selected_line = mix(info_hue, 0.30, bg[5]);

        let mut frame = sidebar;
        let mut page = canvas;
        let mut frame_control = control;
        let mut frame_control_hover = control_hover;
        let mut frame_row_hover = row_hover;
        let mut frame_row_selected = row_selected;
        let mut frame_field = surface_input;
        let mut frame_meta = quiet;
        let mut frame_label = quiet_soft;
        let mut page_shadow = Elevation {
            glint: false,
            layers: vec![
                ShadowLayer { x: 0.0, y: 0.0, blur: 0.0, spread: 1.0, color: line_glass },
                ShadowLayer { x: 0.0, y: 1.0, blur: 3.0, spread: 0.0, color: shade(0.22) },
                ShadowLayer { x: 0.0, y: 8.0, blur: 28.0, spread: 0.0, color: shade(0.2) },
            ],
        };

        if env.material_shows() {
            frame = alpha(sidebar, 0.16 + 0.5 * tint);
            frame_control = ink_wash(0.08);
            frame_control_hover = ink_wash(0.13);
            frame_row_hover = ink_wash(0.065);
            frame_row_selected = ink_wash(0.13);
            frame_field = alpha(bg[0], 0.32);
            frame_meta = quiet_strong;
            frame_label = quiet;
        }

        if env.glass_sheet() {
            frame = alpha(canvas, 0.7 + 0.24 * tint);
            page = TRANSPARENT;
            page_shadow = Elevation::default();
            surface = ink_wash(0.045);
            surface_raised = ink_wash(0.075);
            surface_hover = ink_wash(0.1);
            surface_active = ink_wash(0.13);
            surface_input = alpha(bg[0], 0.3);
            surface_menu = alpha(bg[3], 0.82 + 0.14 * tint);
            surface_menu_hover = ink_wash(0.12);
            sunken = alpha(bg[0], 0.28);
            control = ink_wash(0.1);
            control_soft = ink_wash(0.06);
            control_hover = ink_wash(0.15);
            chip = ink_wash(0.12);
            code_surface = ink_wash(0.07);
            row_hover = ink_wash(0.065);
            row_selected = ink_wash(0.13);
            meter_track = ink_wash(0.14);
            l_line = ink_wash(0.1);
            line_soft = ink_wash(0.065);
            line_mid = ink_wash(0.13);
            line_strong = ink_wash(0.19);
            line_control = ink_wash(0.15);
            line_bright = ink_wash(0.32);
            diff_add_surface = alpha(success_hue, 0.1);
            diff_add_line = alpha(success_hue, 0.26);
            diff_add_gutter = alpha(success_hue, 0.15);
            diff_del_surface = alpha(danger_hue, 0.1);
            diff_del_line = alpha(danger_hue, 0.26);
            diff_del_gutter = alpha(danger_hue, 0.15);
            accent_surface = alpha(accent, 0.1);
            accent_chip = alpha(accent, 0.18);
            accent_line = alpha(accent, 0.32);
            info_surface = alpha(info_hue, 0.17);
            info_surface_soft = alpha(info_hue, 0.1);
            info_line = alpha(info_hue, 0.28);
            success_surface = alpha(success_hue, 0.18);
            danger_surface = alpha(danger_hue, 0.11);
            danger_chip = alpha(danger_hue, 0.18);
            danger_line = alpha(danger_hue, 0.32);
            diff_hunk_surface = ink_wash(0.05);
            diff_absent = ink_wash(0.025);
            diff_selected = alpha(info_hue, 0.18);
            diff_selected_line = alpha(info_hue, 0.32);
        }

        let frame_clear = frame;
        let ground = env.material_ground.filter(|_| env.material_shows()).map(|ground| if dark { ground.dark } else { ground.light });
        if let Some(ground) = ground {
            frame = over(frame, ground);
        }

        let glint = alpha(WHITE, 0.075);
        let elevation = |layers: &[(f32, f32, f32)]| Elevation {
            glint: true,
            layers: layers
                .iter()
                .map(|&(y, blur, weight)| ShadowLayer { x: 0.0, y, blur, spread: 0.0, color: shade(weight) })
                .collect(),
        };

        let (glass, glass_menu, glass_clear, glass_blur, veil, veil_deep, veil_blur);
        let scrim = rgb_triple(p.scrim, 0.85);
        if env.reduced_transparency {
            glass = Pane::solid(surface);
            glass_menu = Pane::solid(surface_menu);
            glass_clear = Pane::solid(surface_menu);
            glass_blur = false;
            veil = scrim;
            veil_deep = scrim;
            veil_blur = false;
        } else {
            glass = Pane::sheened(alpha(bg[2], 0.6 + 0.35 * tint));
            glass_menu = Pane::sheened(alpha(bg[4], 0.6 + 0.35 * tint));
            glass_clear = Pane::sheened(alpha(bg[4], 0.31 + 0.5 * tint));
            glass_blur = true;
            veil = rgb_triple(p.scrim, 0.46);
            veil_deep = rgb_triple(p.scrim, 0.7);
            veil_blur = true;
        }

        let ansi = [
            c(p.ansi_black),
            c(p.ansi_red),
            c(p.ansi_green),
            c(p.ansi_yellow),
            c(p.ansi_blue),
            c(p.ansi_magenta),
            c(p.ansi_cyan),
            c(p.ansi_white),
            c(p.ansi_bright_black),
            c(p.ansi_bright_red),
            c(p.ansi_bright_green),
            c(p.ansi_bright_yellow),
            c(p.ansi_bright_blue),
            c(p.ansi_bright_magenta),
            c(p.ansi_bright_cyan),
            c(p.ansi_bright_white),
        ];
        let worktree_mix = |hue: u32| mix(c(hue), 0.7, fg[2]);
        let info_badge = c(p.info_strong);

        Tokens {
            variant: p.variant,
            canvas,
            canvas_fade: alpha(bg[0], 0.0),
            sunken,
            dock_sunken: if env.glass_sheet() { alpha(sunken, 0.0) } else { sunken },
            sidebar,
            surface,
            surface_raised,
            surface_menu,
            surface_menu_hover,
            surface_hover,
            surface_active,
            row_hover,
            row_selected,
            surface_input,
            surface_glass,
            frame,
            frame_clear,
            ground,
            page,
            frame_control,
            frame_control_hover,
            frame_row_hover,
            frame_row_selected,
            frame_field,
            frame_meta,
            frame_label,
            surface_inverse: c(p.contrast),
            surface_inverse_soft: mix(c(p.contrast), 0.85, bg[6]),
            surface_inverse_off: mix(bg[6], 0.8, fg[5]),
            control,
            control_soft,
            control_hover,
            chip,
            wash,
            code_surface,
            code_ink: c(p.ansi_white),
            meter_fill: fg[4],
            meter_track,

            line: l_line,
            line_soft,
            line_mid,
            line_strong,
            line_bright,
            line_glass,
            line_input,
            line_control,
            scroll_thumb: line_input,

            ink: fg0,
            ink_soft: mix(fg0, 0.5, fg[1]),
            ink_dim: fg[1],
            ink_faint: mix(fg[1], 0.5, fg[2]),
            muted_strong: mix(fg[1], 0.25, fg[2]),
            muted: fg[2],
            muted_soft: mix(fg[2], 0.5, fg[3]),
            quiet_strong,
            quiet,
            quiet_soft,
            disabled_opacity: p.disabled_opacity,
            ink_inverse: c(p.contrast_ink),
            ink_inverse_soft: mix(c(p.contrast_ink), 0.65, c(p.contrast)),

            accent,
            accent_text,
            accent_hover: p.accent_hover.map(c).unwrap_or_else(|| mix(accent, 0.82, fg0)),
            accent_bright: c(p.accent_bright),
            accent_soft: mix(accent_text, 0.7, fg0),
            accent_ink: c(p.accent_ink),
            accent_surface,
            accent_chip,
            accent_line,
            accent_line_strong: mix(accent, 0.42, bg[5]),
            accent_line_soft: mix(accent, 0.15, bg[5]),

            info,
            info_soft: mix(info, 0.7, fg[2]),
            info_surface,
            info_surface_soft,
            info_line,
            info_badge,
            info_ink: c(p.info_ink),
            badge_ink: on_hue,
            focus_ring: accent_text,
            link,
            link_hover: mix(link, 0.7, on_hue),
            link_line: mix(c(p.link), 0.45, bg[5]),
            text_selection: if dark { c(0x3f638b) } else { c(0xb3d7ff) },

            worktree: [
                worktree_mix(p.ansi_blue),
                worktree_mix(p.ansi_green),
                worktree_mix(p.ansi_magenta),
                worktree_mix(p.ansi_yellow),
                worktree_mix(p.ansi_cyan),
                worktree_mix(p.ansi_bright_blue),
            ],
            role_coordinator: mix(c(p.ansi_magenta), 0.55, fg0),
            role_implementer: mix(c(p.ansi_blue), 0.75, fg0),
            role_reviewer: mix(c(p.ansi_yellow), 0.55, fg0),
            role_researcher: mix(c(p.ansi_cyan), 0.55, fg0),

            success,
            success_soft: mix(success, 0.75, fg[3]),
            success_surface,
            success_line: mix(success_hue, 0.5, bg[5]),
            success_ink: mix(success_hue, 0.1, bg[0]),

            danger,
            danger_ink: p.danger_ink.map(c).unwrap_or(c(p.accent_ink)),
            danger_soft: mix(danger, 0.78, fg[2]),
            danger_surface,
            danger_chip,
            danger_line,
            danger_line_hover: mix(danger_hue, 0.45, bg[5]),

            warning,
            pr_merged: c(p.pr_merged),

            alert,
            alert_hover: mix(alert, 0.82, on_hue),
            alert_line: mix(alert, 0.65, on_hue),
            alert_ink: c(p.alert_ink),

            terminal_selection: mix(info_hue, 0.25, bg[5]),
            find_match: mix(c(p.ansi_yellow), 0.25, bg[0]),
            find_active: accent,
            ansi,

            diff_add_surface,
            diff_add_line,
            diff_add_gutter,
            diff_del_surface,
            diff_del_line,
            diff_del_gutter,
            diff_hunk_surface,
            diff_hunk_ink: fg[4],
            diff_gutter_ink: quiet_soft,
            diff_absent,
            diff_selected,
            diff_selected_line,

            syntax_comment: c(p.syntax_comment),
            syntax_string: c(p.syntax_string),
            syntax_number: c(p.syntax_number.unwrap_or(p.ansi_yellow)),
            syntax_constant: c(p.syntax_constant.unwrap_or(p.ansi_yellow)),
            syntax_keyword: c(p.syntax_keyword.unwrap_or(p.ansi_magenta)),
            syntax_operator: c(p.link),
            syntax_function: c(p.syntax_function.unwrap_or(p.ansi_blue)),
            syntax_type: c(p.syntax_type.unwrap_or(p.ansi_cyan)),
            syntax_variable: c(p.ansi_white),
            syntax_tag: c(p.syntax_tag.unwrap_or(p.ansi_red)),
            syntax_attribute: c(p.syntax_attribute.unwrap_or(p.ansi_yellow)),
            syntax_punctuation: c(p.syntax_punctuation),

            glint,
            glass_tint: tint,
            glass,
            glass_menu,
            glass_clear,
            glass_blur,
            glass_strip: alpha(canvas, 0.78 + 0.16 * tint),
            tone_glass_ground: if env.reduced_transparency { surface_menu } else { alpha(bg[2], 0.62) },
            glass_rim: Rim {
                hairline: alpha(WHITE, light_dark(0.15, 0.03)),
                top: alpha(WHITE, light_dark(0.25, 0.1)),
                bottom: alpha(WHITE, light_dark(0.1, 0.03)),
            },
            glass_float_outline: alpha(crate::color::BLACK, light_dark(0.12, 0.35)),
            glass_edge: alpha(WHITE, 0.05),
            glass_control: Pane::sheened(wash),
            glass_control_hover: Pane::sheened(surface_hover),
            glass_pick: Pane::sheened(alpha(accent, 0.26)),
            glass_pick_danger: Pane::sheened(alpha(danger, 0.22)),

            shadow_menu: elevation(&[(2.0, 6.0, 0.2), (14.0, 36.0, 0.34)]),
            shadow_popover: elevation(&[(2.0, 8.0, 0.2), (18.0, 46.0, 0.38)]),
            shadow_panel: elevation(&[(3.0, 10.0, 0.2), (24.0, 64.0, 0.42)]),
            shadow_composer: elevation(&[(1.0, 2.0, 0.14), (15.0, 45.0, 0.2)]),
            shadow_float: elevation(&[(1.0, 2.0, 0.16), (6.0, 16.0, 0.22)]),
            shadow_float_lift: elevation(&[(1.0, 3.0, 0.2), (10.0, 26.0, 0.28)]),
            shadow_overlay: elevation(&[(4.0, 14.0, 0.22), (30.0, 80.0, 0.5)]),
            shadow_page: page_shadow,
            shadow_sidebar: Elevation {
                glint: false,
                layers: vec![ShadowLayer { x: 20.0, y: 0.0, blur: 60.0, spread: 0.0, color: shade(0.45) }],
            },
            shadow_thumb: Elevation {
                glint: false,
                layers: vec![
                    ShadowLayer { x: 0.0, y: 1.0, blur: 2.0, spread: 0.0, color: shade(0.4) },
                    ShadowLayer { x: 0.0, y: 4.0, blur: 10.0, spread: 0.0, color: shade(0.22) },
                ],
            },
            scrim,
            veil,
            veil_deep,
            veil_blur,
            scrim_soft: shade(0.38),

            panel_slide: Motion { millis: 300, ease: SLIDE_EASE },
            glide: Motion { millis: 360, ease: SLIDE_EASE },
            touch: Motion { millis: 140, ease: TOUCH_EASE },
            reduced_motion: env.reduced_motion,
        }
    }

    /// The six roles of a toned primitive, as tone.css defines them.
    pub fn tone(&self, tone: Tone) -> ToneColors {
        match tone {
            Tone::Accent => ToneColors {
                tone: self.accent,
                text: self.accent_text,
                soft: self.accent_soft,
                line: self.accent_line,
                surface: self.accent_surface,
                chip: self.accent_chip,
            },
            Tone::Danger => ToneColors {
                tone: self.danger,
                text: self.danger,
                soft: self.danger_soft,
                line: self.danger_line,
                surface: self.danger_surface,
                chip: self.danger_chip,
            },
            Tone::Info => ToneColors {
                tone: self.info,
                text: self.info,
                soft: self.info_soft,
                line: self.info_line,
                surface: self.info_surface_soft,
                chip: self.info_surface,
            },
            Tone::Success => ToneColors {
                tone: self.success,
                text: self.success,
                soft: self.success_soft,
                line: self.success_line,
                surface: self.success_surface,
                chip: self.success_surface,
            },
            Tone::Warning => ToneColors {
                tone: self.warning,
                text: self.warning,
                soft: self.warning,
                line: alpha(self.warning, 0.32),
                surface: alpha(self.warning, 0.1),
                chip: alpha(self.warning, 0.2),
            },
        }
    }

    /// `color-mix(in srgb, var(--focus-ring) 18%, transparent)`: the soft halo around a focused field.
    pub fn focus_halo(&self) -> Rgba {
        mix(self.focus_ring, 0.18, TRANSPARENT)
    }

    /// The motion a transition should run, or none under reduced motion.
    pub fn motion(&self, motion: Motion) -> Option<Motion> {
        (!self.reduced_motion).then_some(motion)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::palette_for;

    fn hex_of(color: Rgba) -> u32 {
        let byte = |c: f32| (c * 255.0).round() as u32;
        (byte(color.r) << 16) | (byte(color.g) << 8) | byte(color.b)
    }

    #[test]
    fn mixed_tokens_match_the_stylesheet() {
        let tokens = Tokens::derive(&palette_for("aicodingtool-dark"), &Environment::default());
        // color-mix(in srgb, #20242a 40%, #292d34)
        assert_eq!(hex_of(tokens.control_hover), 0x252930);
        // An unset --p-accent-text falls back to the accent.
        assert_eq!(tokens.accent_text, hex(0xd97757));
    }

    #[test]
    fn the_glass_sheet_frame_reads_as_the_electron_window_does() {
        let tokens = Tokens::derive(&palette_for("aicodingtool-dark"), &Environment::macos_glass());
        assert_eq!(hex_of(tokens.frame), 0x272a2e);
        assert_eq!(tokens.page.a, 0.0);
    }
}
