//! Shared drawing primitives for apps with their own screens.

pub use super::image::GrayImage;
use super::{gray, Align as RenderAlign, Renderer, Text, BLACK, WHITE};
use kl_core::{Layout, Rect, Status};
pub use primitive_quick::{icons, Icon};

struct Picture {
    rect: Rect,
    image: GrayImage,
    after_popups: usize,
}

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
    pictures: Vec<Picture>,
    popup_covers: Vec<(Rect, f32)>,
}

impl Canvas {
    pub fn new(width: u32, height: u32, fonts: &[String]) -> Result<Self, String> {
        Ok(Self {
            renderer: Renderer::new(width, height, fonts)?,
            bounds: Rect::new(0, 0, width as i32, height as i32),
            pictures: Vec::new(),
            popup_covers: Vec::new(),
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
        self.popup_covers.clear();
    }

    pub fn finish(&mut self, dark: bool) -> Vec<u8> {
        let width = self.bounds.w as usize;
        let mut image = self.renderer.finish_gray();
        if dark {
            super::image::invert_except(&mut image, width, &[], None);
        }
        for picture in &self.pictures {
            blit_picture(
                &mut image,
                self.bounds,
                picture,
                &self.popup_covers[picture.after_popups..],
            );
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
        let cover = rect.inset(-halo);
        let radius = self.px(38);
        self.popup_covers.push((cover, radius as f32));
        self.renderer.rounded(cover, f64::from(radius), WHITE);
        self.panel(rect, 28, 6, WHITE);
    }

    /// The same clock, connectivity and battery strip as the launcher.
    pub fn status_bar(&mut self, status: &Status, running: usize) {
        let layout = Layout::new(self.bounds.w as u32, self.bounds.h as u32, 4, 5);
        self.renderer.status_strip(&layout, status, running);
    }

    /// An outline with semicircular ends.
    pub fn pill(&mut self, rect: Rect, border: i32, inside: u8) {
        self.renderer.rounded_frame(
            rect,
            f64::from(rect.h.min(rect.w)) / 2.0,
            self.px(border).max(1),
            BLACK,
            inside,
        );
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

    pub fn icon(&mut self, rect: Rect, icon: Icon, ink: u8) {
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
        self.pictures.push(Picture {
            rect,
            image: picture,
            after_popups: self.popup_covers.len(),
        });
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

fn blit_picture(frame: &mut [u8], bounds: Rect, picture: &Picture, covered_by: &[(Rect, f32)]) {
    let Picture { rect, image, .. } = picture;
    if u64::from(image.width) * u64::from(image.height) != image.pixels.len() as u64 {
        return;
    }
    let (Ok(width), Ok(height)) = (i32::try_from(image.width), i32::try_from(image.height)) else {
        return;
    };
    let x_start = rect.x.max(0);
    let x_end = rect.x.saturating_add(rect.w.min(width)).min(bounds.w);
    if x_start >= x_end {
        return;
    }
    let y_end = rect.y.saturating_add(rect.h.min(height)).min(bounds.h);
    let stride = bounds.w as usize;
    for y in rect.y.max(0)..y_end {
        let src_start = (y - rect.y) as usize * width as usize + (x_start - rect.x) as usize;
        let dst_start = y as usize * stride + x_start as usize;
        let len = (x_end - x_start) as usize;
        let source = &image.pixels[src_start..src_start + len];
        let target = &mut frame[dst_start..dst_start + len];
        if covered_by.is_empty() {
            target.copy_from_slice(source);
            continue;
        }
        for (offset, (&pixel, target)) in source.iter().zip(target).enumerate() {
            let x = x_start + offset as i32;
            if !covered_by.iter().any(|(r, radius)| {
                r.contains(x as f32, y as f32)
                    && super::image::rect_distance(x - r.x, y - r.y, r.w, r.h, *radius) < 0.0
            }) {
                *target = pixel;
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pictures_clip_at_screen_edges_and_leave_overlapping_popup_visible() {
        let bounds = Rect::new(0, 0, 6, 4);
        let picture = Picture {
            rect: Rect::new(-2, -1, 7, 5),
            image: GrayImage {
                width: 7,
                height: 5,
                pixels: (0..35).collect(),
            },
            after_popups: 0,
        };
        let mut pixels = vec![99; 24];
        blit_picture(
            &mut pixels,
            bounds,
            &picture,
            &[(Rect::new(2, 1, 2, 3), 0.0)],
        );
        assert_eq!(&pixels[..6], &[9, 10, 11, 12, 13, 99]);
        assert_eq!(&pixels[6..12], &[16, 17, 99, 99, 20, 99]);
        assert_eq!(&pixels[18..], &[30, 31, 99, 99, 34, 99]);
    }
}
