//! A single palette for application surfaces and the component library.
use crate::preferences::{ColorMode, Preferences};
use gpui::{
    component::{Theme, ThemeMode},
    px, rgb, App, Window,
};

pub fn apply(preferences: &Preferences, mut window: Option<&mut Window>, cx: &mut App) {
    let mode = match preferences.color_mode {
        ColorMode::Light => ThemeMode::Light,
        ColorMode::Dark => ThemeMode::Dark,
        ColorMode::System => window
            .as_ref()
            .map(|w| w.appearance())
            .unwrap_or_else(|| cx.window_appearance())
            .into(),
    };
    Theme::change(mode, window.as_deref_mut(), cx);
    let theme = Theme::global_mut(cx);
    let dark = mode.is_dark();
    theme.font_size = px(preferences.font_size);
    theme.radius = px(7.);
    theme.radius_lg = px(10.);
    theme.shadow = true;
    theme.focus_ring = true;
    let c = &mut theme.colors;
    c.background = rgb(if dark { 0x181c24 } else { 0xffffff }).into();
    c.foreground = rgb(if dark { 0xe8edf5 } else { 0x202939 }).into();
    c.sidebar = rgb(if dark { 0x12161d } else { 0xf3f5f9 }).into();
    c.sidebar_foreground = c.foreground;
    c.secondary = rgb(if dark { 0x252c38 } else { 0xeef2f8 }).into();
    c.secondary_foreground = c.foreground;
    c.muted = c.secondary;
    c.muted_foreground = rgb(if dark { 0xa4b0c2 } else { 0x657186 }).into();
    c.border = rgb(if dark { 0x364151 } else { 0xdce3ee }).into();
    c.input = c.border;
    c.primary = rgb(if dark { 0x8caaff } else { 0x315bce }).into();
    c.primary_hover = rgb(if dark { 0xa4baff } else { 0x264aaf }).into();
    c.primary_active = rgb(if dark { 0x789bf6 } else { 0x1e3d96 }).into();
    c.primary_foreground = rgb(if dark { 0x111c39 } else { 0xffffff }).into();
    c.button_primary = c.primary;
    c.button_primary_hover = c.primary_hover;
    c.button_primary_active = c.primary_active;
    c.button_primary_foreground = c.primary_foreground;
    c.button = c.background;
    c.button_foreground = c.foreground;
    c.button_hover = c.secondary;
    c.button_active = c.secondary;
    c.ring = c.primary;
    c.progress_bar = c.primary;
    c.slider_bar = c.primary;
    c.sidebar_accent = rgb(if dark { 0x263550 } else { 0xe3ebff }).into();
    c.sidebar_accent_foreground = c.primary;
    c.accent = c.secondary;
    c.accent_foreground = c.foreground;
    c.popover = rgb(if dark { 0x212733 } else { 0xffffff }).into();
    c.popover_foreground = c.foreground;
    theme.tokens = (&theme.colors).into();
    theme.motion = Default::default();
    if preferences.reduced_motion {
        theme.motion.duration_fast = std::time::Duration::ZERO;
        theme.motion.duration_normal = std::time::Duration::ZERO;
        theme.motion.duration_slow = std::time::Duration::ZERO;
    }
    Theme::sync_base(cx);
    if let Some(window) = window {
        window.set_rem_size(px(preferences.font_size));
    }
    cx.refresh_windows();
}
