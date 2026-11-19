//! The e-ink display for the Kindle apps: one panel, with full updates on request and small updates otherwise.

use std::time::Instant;

use kl_core::Settings;

use primitive_device::{log, profile};

use crate as eink;

const W: u32 = profile::ACTIVE.screen_w;
const H: u32 = profile::ACTIVE.screen_h;

/// Shows frames on the panel: a full GC16 update on request, otherwise only the box around the changed
/// pixels, with a non-flashing waveform (DU for pure black and white, AUTO when icons bring gray). A tap on one
/// icon updates that icon, not the full-width band of its row.
pub struct Display {
    panel: Option<eink::Panel>,
    last: Vec<u8>,
    presented_frame: bool,
}

impl Display {
    pub fn open() -> Display {
        let panel = match eink::Panel::open() {
            Ok(p) => Some(p),
            Err(e) => {
                log(&format!("panel: {e}"));
                None
            }
        };
        Display { panel, last: Vec::new(), presented_frame: false }
    }

    pub fn fb_turn(&self) -> u32 {
        self.panel.as_ref().map_or(0, |p| p.turn())
    }

    pub fn has_presented_frame(&self) -> bool {
        self.presented_frame
    }

    pub fn show(&mut self, upright: Vec<u8>, settings: &Settings, full: bool) {
        let fb_turn = self.fb_turn();
        let frame = kl_render::for_panel(upright, settings.rotation, fb_turn);
        let Some(panel) = self.panel.as_mut() else {
            self.last = frame;
            return;
        };
        let t = Instant::now();
        let result = if full || self.last.len() != frame.len() {
            panel.draw(W, (0, 0, W, H), &frame, eink::WAVE_GC16, true).map(|ok| (ok, "GC16 full", (0, 0, W, H)))
        } else if let Some((x, y, w, h)) = kl_render::changed_rect(&self.last, &frame, W as usize) {
            let region = (x as u32, y as u32, w as u32, h as u32);
            let wave = region_wave(&frame, region);
            let name = if wave == eink::WAVE_DU { "DU" } else { "AUTO" };
            panel.draw(W, region, &frame, wave, false).map(|ok| (ok, name, region))
        } else {
            Ok((true, "unchanged", (0, 0, 0, 0)))
        };
        match result {
            Ok((true, wave, (x, y, w, h))) => {
                self.presented_frame |= w > 0 && h > 0;
                self.last = frame;
                log(&format!("draw {wave} {w}x{h} at {x},{y} in {} ms (fb turn {fb_turn})", t.elapsed().as_millis()));
            }
            Ok((false, ..)) => {
                self.last.clear();
                log("draw: the framebuffer is not in a drawable state");
            }
            Err(e) => {
                self.last.clear();
                log(&format!("draw: {e}"));
            }
        }
    }

    /// Show only `area` of `upright` (a pop-up), leaving the rest of the panel as it is. The next `show` draws
    /// the whole screen, as what is on the panel is no longer known.
    pub fn show_region(&mut self, upright: Vec<u8>, area: kl_core::Rect, settings: &Settings) {
        let fb_turn = self.fb_turn();
        let turned = (settings.rotation.turn() + fb_turn) % 4 == 2;
        let frame = kl_render::for_panel(upright, settings.rotation, fb_turn);
        self.last.clear();
        let Some(panel) = self.panel.as_mut() else { return };
        // Upside down, the area sits at the opposite corner of the panel.
        let (x, y) = if turned { (W as i32 - area.right(), H as i32 - area.bottom()) } else { (area.x, area.y) };
        let x0 = x.clamp(0, W as i32) as u32 / 8 * 8;
        let x1 = ((x + area.w).clamp(0, W as i32) as u32).div_ceil(8) * 8;
        let (y0, y1) = (y.clamp(0, H as i32) as u32, (y + area.h).clamp(0, H as i32) as u32);
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let region = (x0, y0, x1.min(W) - x0, y1 - y0);
        let wave = region_wave(&frame, region);
        if let Err(e) = panel.draw(W, region, &frame, wave, false) {
            log(&format!("draw: {e}"));
        }
    }
}

fn region_wave(frame: &[u8], (x, y, w, h): (u32, u32, u32, u32)) -> u32 {
    let stride = W as usize;
    let (left, right) = (x as usize, (x + w) as usize);
    let black_and_white = (y as usize..(y + h) as usize)
        .all(|row| kl_render::is_black_and_white(&frame[row * stride + left..row * stride + right]));
    if black_and_white { eink::WAVE_DU } else { eink::WAVE_AUTO }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rectangle_waveform_depends_only_on_the_region_pixels() {
        let mut frame = vec![255; W as usize * 2];
        frame[0] = 128;
        assert_eq!(region_wave(&frame, (8, 0, 8, 2)), eink::WAVE_DU);
        frame[W as usize + 8] = 128;
        assert_eq!(region_wave(&frame, (8, 0, 8, 2)), eink::WAVE_AUTO);
    }
}

