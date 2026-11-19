//! Shared drawing primitives for apps with their own screens.

use super::{gray, Align as RenderAlign, GrayImage, Renderer, Text, BLACK, WHITE};
use kl_core::{Layout, Rect};

/// Text alignment inside its bounds.
#[derive(Clone, Copy)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// Draws a reusable component screen with the launcher's font and pixel pipeline.
pub struct Canvas {
    renderer: Renderer,
    bounds: Rect,
    pictures: Vec<(Rect, GrayImage)>,
}

impl Canvas {
    pub fn new(width: u32, height: u32, fonts: &[String]) -> Result<Self, String> {
        Ok(Self {
            renderer: Renderer::new(width, height, fonts)?,
            bounds: Rect::new(0, 0, width as i32, height as i32),
            pictures: Vec::new(),
        })
    }

    pub fn bounds(&self) -> Rect {
        self.bounds
    }

    /// Scale a measurement from the reference width of 1264 pixels.
    pub fn px(&self, pixels: i32) -> i32 {
        ((pixels as f64 * f64::from(self.bounds.w)) / 1264.0).round() as i32
    }

    pub fn begin(&mut self) {
        self.renderer.ctx.reset();
        self.renderer.fill(self.bounds, WHITE);
        self.pictures.clear();
    }

    pub fn finish(&mut self, dark: bool) -> Vec<u8> {
        let width = self.bounds.w as usize;
        let mut image = self.renderer.finish_gray();
        if dark {
            super::image::invert_except(&mut image, width, &[], None);
        }
        for (rect, picture) in &self.pictures {
            super::blit(&mut image, width, *rect, picture);
        }
        image
    }

    pub fn fill(&mut self, rect: Rect, ink: u8) {
        self.renderer.fill(rect, ink);
    }

    pub fn circle(&mut self, rect: Rect, ink: u8) {
        self.renderer
            .rounded(rect, f64::from(rect.h.min(rect.w)) / 2.0, ink);
    }

    pub fn card(&mut self, rect: Rect) {
        self.panel(rect, 22, 2, WHITE);
    }

    pub fn popup(&mut self, rect: Rect) {
        let halo = self.px(10);
        self.renderer
            .rounded(rect.inset(-halo), f64::from(self.px(38)), WHITE);
        self.panel(rect, 28, 6, WHITE);
    }

    pub fn panel(&mut self, rect: Rect, radius: i32, border: i32, inside: u8) {
        self.renderer.rounded_frame(
            rect,
            f64::from(self.px(radius)),
            self.px(border).max(1),
            BLACK,
            inside,
        );
    }

    pub fn text(&mut self, rect: Rect, content: &str, style: TextStyle) {
        if rect.w <= 0 || rect.h <= 0 || style.lines == 0 {
            return;
        }
        let align = match style.align {
            Align::Left => RenderAlign::Start,
            Align::Center => RenderAlign::Center,
            Align::Right => RenderAlign::End,
        };
        let clip = super::kurbo::Shape::to_path(&super::krect(rect), 0.1);
        self.renderer.ctx.set_transform(super::Affine::IDENTITY);
        self.renderer.ctx.push_clip_layer(&clip);
        self.renderer.text(Text {
            text: content,
            x: rect.x as f32,
            y: rect.y as f32,
            w: rect.w as f32,
            size: self.px(style.size).max(1) as f32,
            bold: style.bold,
            align,
            lines: style.lines,
            gray: style.ink,
        });
        self.renderer.ctx.pop_layer();
    }

    pub fn measure(&mut self, content: &str, size: i32) -> i32 {
        self.renderer
            .text_width(content, self.px(size).max(1) as f32, false)
            .ceil() as i32
    }

    pub fn button(&mut self, rect: Rect, label: &str, primary: bool, pressed: bool) {
        let filled = primary != pressed;
        self.panel(rect, 16, 4, if filled { BLACK } else { WHITE });
        let top = (rect.h - self.px(45)).max(0) / 2;
        self.text(
            Rect::new(
                rect.x + self.px(8),
                rect.y + top,
                rect.w - self.px(16),
                rect.h - top,
            ),
            label,
            TextStyle::new(36)
                .bold()
                .center()
                .ink(if filled { WHITE } else { BLACK }),
        );
    }

    pub fn icon(&mut self, rect: Rect, icon: primitive_quick::Icon, ink: u8) {
        self.renderer.ctx.set_paint(gray(ink));
        self.renderer.draw_icon(
            icon,
            rect.x.into(),
            rect.y.into(),
            rect.w.min(rect.h).into(),
            ink,
        );
    }

    pub fn picture(&mut self, rect: Rect, picture: GrayImage) {
        self.pictures.push((rect, picture));
    }

    pub fn keyboard(
        &mut self,
        layer: primitive_keyboard::Layer,
        done: &str,
        pressed: Option<Rect>,
    ) {
        let layout = Layout::new(self.bounds.w as u32, self.bounds.h as u32, 4, 5);
        let band = layout.keyboard_area();
        self.fill(band, WHITE);
        self.fill(Rect::new(band.x, band.y, band.w, 2), 226);
        for (key, rect) in layout.keyboard_keys_with(layer, done) {
            self.renderer.key(&key, rect, pressed == Some(rect));
        }
    }
}

#[derive(Clone, Copy)]
pub struct TextStyle {
    size: i32,
    bold: bool,
    align: Align,
    lines: usize,
    ink: u8,
}

impl TextStyle {
    pub const fn new(size: i32) -> Self {
        Self {
            size,
            bold: false,
            align: Align::Left,
            lines: 1,
            ink: BLACK,
        }
    }

    pub const fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    pub const fn center(mut self) -> Self {
        self.align = Align::Center;
        self
    }

    pub const fn right(mut self) -> Self {
        self.align = Align::Right;
        self
    }

    pub const fn lines(mut self, lines: usize) -> Self {
        self.lines = lines;
        self
    }

    pub const fn ink(mut self, ink: u8) -> Self {
        self.ink = ink;
        self
    }
}
