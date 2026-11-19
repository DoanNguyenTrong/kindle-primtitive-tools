//! Copied from Primitive Browser 0.2.12 (`crates/kb-kindle/src/eink.rs`), with `Panel::turn` added.
//!
//! Drawing to the e-ink panel in-process: the pixels are copied into the framebuffer and the update
//! ioctl is sent from here, as the Kindle's own reader does, instead of starting fbink for every update.
//!
//! Two reasons, both measured on the Oasis 3 with the kernel's e-ink debug log:
//!
//! - Starting fbink costs about 170 ms an update (process start, device detection, BMP decode). With the
//!   frame file and the rest, a page turn took 280-1340 ms from button press to update; the stock reader
//!   takes 60-100 ms.
//! - For AUTO, fbink tells the driver to use GC16, the flashing waveform, for any update with gray in it
//!   (`hist_gray_waveform_mode`). Anti-aliased text always has gray, so every page turn flashed black (white
//!   in dark mode). Here gray content gets REAGL, the non-flashing page-turn waveform.
//!
//! The structure and ioctl are the Oasis 3 (Zelda platform) ones, from FBInk's `eink/mxcfb-kindle.h`.

pub mod display;
pub use display::Display;

use std::{
    fs::{File, OpenOptions},
    io,
    os::unix::io::AsRawFd,
};

const FRAMEBUFFER: &str = "/dev/fb0";

/// Waveform codes on the Zelda platform (Oasis 2 and 3). The kernel log prints these numbers.
pub const WAVE_DU: u32 = 1;
pub const WAVE_GC16: u32 = 2;
#[allow(dead_code)]
pub const WAVE_GL16: u32 = 3;
#[allow(dead_code)]
pub const WAVE_REAGL: u32 = 4;
pub const WAVE_AUTO: u32 = 257;

/// The waveform code for a name from the Display sheet.
#[allow(dead_code)]
pub fn wave_code(name: &str) -> Option<u32> {
    match name {
        "AUTO" => Some(WAVE_AUTO),
        "DU" => Some(WAVE_DU),
        "GC16" => Some(WAVE_GC16),
        "GL16" => Some(WAVE_GL16),
        "REAGL" => Some(WAVE_REAGL),
        _ => None,
    }
}

/// The waveforms the driver picks between for an AUTO update: one for pure black and white content,
/// one for content with gray. Only AUTO reads them. REAGL asks for REAGL in both, as fbink does.
fn hist_waves(wave: u32) -> (u32, u32) {
    if wave == WAVE_REAGL {
        (WAVE_REAGL, WAVE_REAGL)
    } else {
        (WAVE_DU, WAVE_REAGL)
    }
}

const UPDATE_MODE_PARTIAL: u32 = 0;
const UPDATE_MODE_FULL: u32 = 1;
/// Let the driver read the panel's temperature itself (the kernel log shows `temp=4096`).
const TEMP_USE_AMBIENT: i32 = 0x1000;

#[repr(C)]
#[derive(Default, Clone, Copy, Debug)]
struct Rect {
    top: u32,
    left: u32,
    width: u32,
    height: u32,
}

#[repr(C)]
#[derive(Default, Clone, Copy, Debug)]
struct AltBuffer {
    phys_addr: u32,
    width: u32,
    height: u32,
    region: Rect,
}

/// `struct mxcfb_update_data_zelda`.
#[repr(C)]
#[derive(Default, Clone, Copy, Debug)]
struct UpdateZelda {
    region: Rect,
    waveform_mode: u32,
    update_mode: u32,
    update_marker: u32,
    temp: i32,
    flags: u32,
    dither_mode: i32,
    quant_bit: i32,
    alt_buffer: AltBuffer,
    hist_bw_waveform_mode: u32,
    hist_gray_waveform_mode: u32,
    ts_pxp: u32,
    ts_epdc: u32,
}

/// `_IOW(kind, nr, size)` on Linux.
const fn iow(kind: u8, nr: u8, size: usize) -> u32 {
    (1u32 << 30) | ((size as u32) << 16) | ((kind as u32) << 8) | nr as u32
}

/// `MXCFB_SEND_UPDATE_ZELDA = _IOW('F', 0x2E, struct mxcfb_update_data_zelda)`.
const MXCFB_SEND_UPDATE_ZELDA: u32 = iow(b'F', 0x2E, std::mem::size_of::<UpdateZelda>());
const FBIOGET_VSCREENINFO: u32 = 0x4600;
const FBIOGET_FSCREENINFO: u32 = 0x4602;

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct Bitfield {
    offset: u32,
    length: u32,
    msb_right: u32,
}

/// `struct fb_var_screeninfo`.
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct VarInfo {
    xres: u32,
    yres: u32,
    xres_virtual: u32,
    yres_virtual: u32,
    xoffset: u32,
    yoffset: u32,
    bits_per_pixel: u32,
    grayscale: u32,
    red: Bitfield,
    green: Bitfield,
    blue: Bitfield,
    transp: Bitfield,
    nonstd: u32,
    activate: u32,
    height: u32,
    width: u32,
    accel_flags: u32,
    pixclock: u32,
    left_margin: u32,
    right_margin: u32,
    upper_margin: u32,
    lower_margin: u32,
    hsync_len: u32,
    vsync_len: u32,
    sync: u32,
    vmode: u32,
    rotate: u32,
    colorspace: u32,
    reserved: [u32; 4],
}

/// `struct fb_fix_screeninfo`.
#[repr(C)]
#[derive(Clone, Copy)]
struct FixInfo {
    id: [u8; 16],
    smem_start: libc::c_ulong,
    smem_len: u32,
    kind: u32,
    kind_aux: u32,
    visual: u32,
    xpanstep: u16,
    ypanstep: u16,
    ywrapstep: u16,
    line_length: u32,
    mmio_start: libc::c_ulong,
    mmio_len: u32,
    accel: u32,
    capabilities: u16,
    reserved: [u16; 2],
}

/// The framebuffer, mapped for writing, and the update counter.
pub struct Panel {
    file: File,
    map: *mut u8,
    len: usize,
    line_length: usize,
    marker: u32,
}

impl Panel {
    /// Open and map the framebuffer. Fails off the Kindle, or when the framebuffer is not 8-bit gray.
    pub fn open() -> Result<Panel, String> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(FRAMEBUFFER)
            .map_err(|e| format!("open {FRAMEBUFFER}: {e}"))?;
        let fd = file.as_raw_fd();
        // SAFETY: FixInfo is plain data; the ioctl fills it.
        let mut fix: FixInfo = unsafe { std::mem::zeroed() };
        if unsafe { libc::ioctl(fd, FBIOGET_FSCREENINFO as _, &mut fix) } < 0 {
            return Err(format!("FBIOGET_FSCREENINFO: {}", io::Error::last_os_error()));
        }
        let var = var_info(fd)?;
        if var.bits_per_pixel != 8 {
            return Err(format!("framebuffer is {} bits per pixel, not 8", var.bits_per_pixel));
        }
        let len = fix.smem_len as usize;
        // SAFETY: mapping the framebuffer this process opened; the length is the driver's own.
        let map = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        if map == libc::MAP_FAILED {
            return Err(format!("mmap: {}", io::Error::last_os_error()));
        }
        Ok(Panel { file, map: map as *mut u8, len, line_length: fix.line_length as usize, marker: 0 })
    }

    /// Copy the region (`x`, `y`, `w`, `h`) of `frame` (a whole screen of `width` gray pixels a row) to the
    /// framebuffer and send the update for that region only, with `wave`; `full` drives every pixel of it (a
    /// flash with GC16). An upside-down screen (rotation 2) is drawn as it is: the panel shows the memory
    /// unturned. Returns `Ok(false)` without drawing when this path cannot draw the screen as it is
    /// (landscape, or another size), so the caller can use fbink.
    pub fn draw(&mut self, width: u32, region: (u32, u32, u32, u32), frame: &[u8], wave: u32, full: bool) -> Result<bool, String> {
        let (x, y, w, rows) = region;
        let fd = self.file.as_raw_fd();
        let var = var_info(fd)?;
        let fits = (var.rotate == 0 || var.rotate == 2)
            && var.xoffset == 0
            && var.xres == width
            && x + w <= width
            && y + rows <= var.yres
            && frame.len() >= (width * (y + rows)) as usize;
        if !fits || rows == 0 || w == 0 {
            return Ok(false);
        }
        let (stride, x, w) = (width as usize, x as usize, w as usize);
        let base = var.yoffset as usize * self.line_length;
        let last_end = base + (y as usize + rows as usize - 1) * self.line_length + x + w;
        if stride > self.line_length || last_end > self.len {
            return Ok(false);
        }
        for r in y as usize..(y + rows) as usize {
            let dst = base + r * self.line_length + x;
            // SAFETY: the destination run is inside the mapping (checked above) and the source run inside `frame`.
            unsafe { std::ptr::copy_nonoverlapping(frame.as_ptr().add(r * stride + x), self.map.add(dst), w) };
        }
        let width = w as u32;
        self.marker = self.marker.wrapping_add(1).max(1);
        let (hist_bw, hist_gray) = hist_waves(wave);
        let mut update = UpdateZelda {
            region: Rect { top: y, left: x as u32, width, height: rows },
            waveform_mode: wave,
            update_mode: if full { UPDATE_MODE_FULL } else { UPDATE_MODE_PARTIAL },
            update_marker: self.marker,
            temp: TEMP_USE_AMBIENT,
            hist_bw_waveform_mode: hist_bw,
            hist_gray_waveform_mode: hist_gray,
            ..UpdateZelda::default()
        };
        // SAFETY: the driver reads an UpdateZelda; the ioctl number carries its size.
        if unsafe { libc::ioctl(fd, MXCFB_SEND_UPDATE_ZELDA as _, &mut update) } < 0 {
            return Err(format!("MXCFB_SEND_UPDATE: {}", io::Error::last_os_error()));
        }
        Ok(true)
    }
}

impl Panel {
    /// The framebuffer's own rotation (0 upright, 2 upside down, 1 and 3 landscape).
    pub fn turn(&self) -> u32 {
        var_info(self.file.as_raw_fd()).map_or(0, |v| v.rotate)
    }
}

impl Drop for Panel {
    fn drop(&mut self) {
        // SAFETY: unmapping the mapping made in `open`.
        unsafe { libc::munmap(self.map as *mut libc::c_void, self.len) };
    }
}

fn var_info(fd: i32) -> Result<VarInfo, String> {
    let mut var = VarInfo::default();
    // SAFETY: VarInfo is plain data; the ioctl fills it.
    if unsafe { libc::ioctl(fd, FBIOGET_VSCREENINFO as _, &mut var) } < 0 {
        return Err(format!("FBIOGET_VSCREENINFO: {}", io::Error::last_os_error()));
    }
    Ok(var)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_struct_matches_the_zelda_header() {
        // 4 (rect) + 7 + 7 (alt buffer: 3 + rect) + 4 words.
        assert_eq!(std::mem::size_of::<UpdateZelda>(), 88);
        assert_eq!(MXCFB_SEND_UPDATE_ZELDA, 0x4058_462E);
        assert_eq!(std::mem::size_of::<VarInfo>(), 160);
    }

    #[test]
    fn auto_asks_for_reagl_on_gray_not_gc16() {
        assert_eq!(hist_waves(WAVE_AUTO), (WAVE_DU, WAVE_REAGL));
        assert_eq!(hist_waves(WAVE_REAGL), (WAVE_REAGL, WAVE_REAGL));
        assert_eq!(wave_code("AUTO"), Some(257));
        assert_eq!(wave_code("GL16"), Some(3));
        assert_eq!(wave_code("nope"), None);
    }
}
