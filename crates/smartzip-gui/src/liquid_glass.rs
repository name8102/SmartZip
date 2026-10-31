//! Opt-in, cross-platform glass prototype. Samples only our own backdrop texture;
//! never captures the desktop, archive contents, or another application.
mod material;

use gpui::{
    canvas,
    component::{ActiveTheme, Theme},
    div,
    prelude::*,
    px, rgba, App, Bounds, BoxShadow, Corners, Div, Pixels, RenderImage, Size, Window,
};
use image::{Frame, RgbaImage};
use material::{Rect, Scene};
use std::{
    cell::RefCell,
    collections::HashMap,
    rc::Rc,
    sync::{Arc, OnceLock},
};

pub fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("SMARTZIP_GUI_STYLE").as_deref() == Ok("liquid-glass"))
}

/// Rebuilt by the normal appearance flow, including when the OS theme changes.
pub fn adapt_theme(theme: &mut Theme) {
    let dark = theme.is_dark();
    theme.radius = px(18.);
    theme.radius_lg = px(24.);
    let c = &mut theme.colors;
    c.background = gpui::rgb(if dark { 0x1a202b } else { 0xf7f9fc }).into();
    c.foreground = gpui::rgb(if dark { 0xeaf0fa } else { 0x203047 }).into();
    c.muted_foreground = gpui::rgb(if dark { 0xb3bfd2 } else { 0x53647a }).into();
    c.secondary = gpui::rgb(if dark { 0x2a3342 } else { 0xe8eef6 }).into();
    c.secondary_foreground = c.foreground;
    c.muted = c.secondary;
    c.border = gpui::rgb(if dark { 0x414e62 } else { 0xd6dfe9 }).into();
    c.input = c.border;
    c.primary = gpui::rgb(if dark { 0x94bfff } else { 0x2663d4 }).into();
    c.primary_hover = gpui::rgb(if dark { 0xb2d1ff } else { 0x2056bc }).into();
    c.primary_active = gpui::rgb(if dark { 0x7eaff7 } else { 0x1b49a1 }).into();
    c.primary_foreground = gpui::rgb(if dark { 0x152844 } else { 0xffffff }).into();
    c.button_primary = c.primary;
    c.button_primary_hover = c.primary_hover;
    c.button_primary_active = c.primary_active;
    c.button_primary_foreground = c.primary_foreground;
    c.button = gpui::rgb(if dark { 0x323d4e } else { 0xfafbfe }).into();
    c.button_foreground = c.foreground;
    c.button_hover = c.secondary;
    c.button_active = gpui::rgb(if dark { 0x3a4960 } else { 0xdce6f4 }).into();
    // Button manages hover/active itself. Do not restyle it with .hover() or
    // instance .bg(), which would overwrite selected/disabled state fills.
    c.accent = c.primary.opacity(if dark { 0.16 } else { 0.10 });
    c.accent_foreground = c.foreground;
    c.secondary_hover = c.foreground.opacity(if dark { 0.08 } else { 0.05 });
    c.secondary_active = c.accent;
    c.button_secondary_hover = c.secondary_hover;
    c.button_secondary_active = c.accent;
    c.sidebar_accent = c.accent;
    c.sidebar_accent_foreground = c.primary;
    c.ring = c.primary;
    c.progress_bar = c.primary;
    c.slider_bar = c.primary;
    c.popover = c.background;
    c.popover_foreground = c.foreground;
}

#[derive(Default, Clone)]
pub struct Glass(Rc<RefCell<Backdrop>>);

#[derive(Default)]
struct Backdrop {
    variant: u8,
    key: Option<(u32, u32, bool, u8)>,
    scale: [f32; 2],
    scene: Option<Scene>,
    image: Option<Arc<RenderImage>>,
    panes: HashMap<&'static str, Pane>,
}

struct Pane {
    key: [i32; 6],
    image: Arc<RenderImage>,
}

fn render_image(mut image: RgbaImage) -> Arc<RenderImage> {
    // GPUI's atlas expects BGRA, image's software operations use RGBA.
    for pixel in image.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    Arc::new(RenderImage::new(vec![Frame::new(image)]))
}

impl Glass {
    /// Raster work is bounded to a 960px texture and cached, not repeated on
    /// hover or every frame. Geometry is in window coordinates, not screen ones.
    pub fn prepare(&self, viewport: Size<Pixels>, window: &mut Window, cx: &App) {
        let mut backdrop = self.0.borrow_mut();
        let width = f32::from(viewport.width).max(1.);
        let height = f32::from(viewport.height).max(1.);
        let scale = (960. / width.max(height)).min(1.);
        let w = (width * scale).round().max(1.) as u32;
        let h = (height * scale).round().max(1.) as u32;
        let key = (w, h, cx.theme().is_dark(), backdrop.variant);
        backdrop.scale = [w as f32 / width, h as f32 / height];
        if backdrop.key == Some(key) {
            return;
        }
        // Explicitly evict replaced rasters from the GPU atlas on resize/theme
        // changes; merely dropping the Arc leaves atlas entries allocated.
        for (_, pane) in backdrop.panes.drain() {
            let _ = window.drop_image(pane.image);
        }
        if let Some(image) = backdrop.image.take() {
            let _ = window.drop_image(image);
        }
        let scene = Scene::new(w, h, key.2, key.3);
        backdrop.image = Some(render_image(scene.background().clone()));
        backdrop.scene = Some(scene);
        backdrop.key = Some(key);
    }

    pub fn next_backdrop(&self) {
        let mut backdrop = self.0.borrow_mut();
        backdrop.variant = (backdrop.variant + 1) % 3;
    }

    pub fn background(&self) -> impl IntoElement {
        let image = self.0.borrow().image.clone();
        canvas(
            |_, _, _| (),
            move |bounds, (), window, _| {
                if let Some(image) = image {
                    let _ =
                        window.paint_image(bounds, bounds, Corners::all(px(0.)), image, 0, false);
                }
            },
        )
        .absolute()
        .inset_0()
        .size_full()
    }

    /// The first child paints the refracted backdrop; subsequent children are
    /// normal GPUI controls. Text/icons are never included in the blur texture.
    pub fn surface(&self, id: &'static str, radius: f32, tint: f32, cx: &App) -> Div {
        let state = self.clone();
        let dark = cx.theme().is_dark();
        div()
            .relative()
            .rounded(px(radius))
            .shadow(vec![
                BoxShadow::new(
                    px(0.),
                    px(8.),
                    rgba(if dark { 0x00000030 } else { 0x26364b13 }).into(),
                )
                .blur_radius(px(24.))
                .spread_radius(px(-6.)),
                BoxShadow::new(
                    px(0.),
                    px(1.),
                    rgba(if dark { 0xffffff16 } else { 0xffffffdd }).into(),
                )
                .spread_radius(px(0.5))
                .inset(),
            ])
            .child(
                canvas(
                    move |bounds, window, _| state.pane(id, bounds, radius, tint, window),
                    move |bounds, image, window, _| {
                        if let Some(image) = image {
                            let _ = window.paint_image(
                                bounds,
                                bounds,
                                Corners::all(px(radius)),
                                image,
                                0,
                                false,
                            );
                        }
                    },
                )
                .absolute()
                .inset_0()
                .size_full(),
            )
    }

    fn pane(
        &self,
        id: &'static str,
        bounds: Bounds<Pixels>,
        radius: f32,
        tint: f32,
        window: &mut Window,
    ) -> Option<Arc<RenderImage>> {
        let mut backdrop = self.0.borrow_mut();
        let [sx, sy] = backdrop.scale;
        let rect = Rect {
            x: f32::from(bounds.origin.x) * sx,
            y: f32::from(bounds.origin.y) * sy,
            width: f32::from(bounds.size.width) * sx,
            height: f32::from(bounds.size.height) * sy,
        };
        let key = [
            (rect.x * 4.).round() as i32,
            (rect.y * 4.).round() as i32,
            rect.width.round() as i32,
            rect.height.round() as i32,
            (radius * sx * 4.).round() as i32,
            (tint * 100.).round() as i32,
        ];
        if let Some(pane) = backdrop.panes.get(id).filter(|pane| pane.key == key) {
            return Some(pane.image.clone());
        }
        let scene = backdrop.scene.as_ref()?;
        let image = render_image(scene.refract(rect, radius * sx, tint));
        if let Some(previous) = backdrop.panes.insert(
            id,
            Pane {
                key,
                image: image.clone(),
            },
        ) {
            let _ = window.drop_image(previous.image);
        }
        Some(image)
    }
}
