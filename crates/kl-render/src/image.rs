//! Gray images for app icons: PNG decoding, scaling into the icon box, dithering to the panel's 16 gray
//! steps, and a small cache file format so icons are not decoded on every start.

use std::io::Cursor;

/// An 8-bit gray image, rows top to bottom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrayImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl GrayImage {
    pub fn blank(width: u32, height: u32) -> GrayImage {
        GrayImage { width, height, pixels: vec![255; (width * height) as usize] }
    }
}

/// Largest PNG we decode (pixels). The Kindle's library icons are about 1000 × 1500.
const MAX_PIXELS: u64 = 4_000_000;

/// Decode a PNG into gray, compositing any transparency over white.
pub fn decode_png(bytes: &[u8]) -> Result<GrayImage, String> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(|e| format!("png: {e}"))?;
    let (w, h) = (reader.info().width, reader.info().height);
    if w == 0 || h == 0 || w as u64 * h as u64 > MAX_PIXELS {
        return Err(format!("png: size {w}x{h} not supported"));
    }
    let mut buf = vec![0; reader.output_buffer_size()];
    let frame = reader.next_frame(&mut buf).map_err(|e| format!("png: {e}"))?;
    let data = &buf[..frame.buffer_size()];
    let channels = match frame.color_type {
        png::ColorType::Grayscale => 1,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        png::ColorType::Indexed => return Err("png: palette not expanded".into()),
    };
    let stride = frame.line_size;
    let mut pixels = Vec::with_capacity((w * h) as usize);
    for y in 0..h as usize {
        let row = &data[y * stride..];
        for x in 0..w as usize {
            let p = &row[x * channels..x * channels + channels];
            let (luma, alpha) = match channels {
                1 => (p[0] as u32, 255),
                2 => (p[0] as u32, p[1] as u32),
                3 => (luma(p[0], p[1], p[2]), 255),
                _ => (luma(p[0], p[1], p[2]), p[3] as u32),
            };
            pixels.push(((luma * alpha + 255 * (255 - alpha)) / 255) as u8);
        }
    }
    Ok(GrayImage { width: w, height: h, pixels })
}

fn luma(r: u8, g: u8, b: u8) -> u32 {
    (r as u32 * 299 + g as u32 * 587 + b as u32 * 114) / 1000
}

/// Scale `img` to fit inside `w` × `h` keeping its shape, centred on white. Each output pixel is the
/// average of the source area it covers, so large icons shrink without sparkle.
pub fn fit(img: &GrayImage, w: u32, h: u32) -> GrayImage {
    let mut out = GrayImage::blank(w, h);
    if img.width == 0 || img.height == 0 || w == 0 || h == 0 {
        return out;
    }
    let scale = (w as f64 / img.width as f64).min(h as f64 / img.height as f64);
    let (dw, dh) = (((img.width as f64 * scale).round() as u32).clamp(1, w), ((img.height as f64 * scale).round() as u32).clamp(1, h));
    let (ox, oy) = ((w - dw) / 2, (h - dh) / 2);
    for dy in 0..dh {
        let sy0 = dy as f64 / scale;
        let sy1 = ((dy + 1) as f64 / scale).min(img.height as f64);
        for dx in 0..dw {
            let sx0 = dx as f64 / scale;
            let sx1 = ((dx + 1) as f64 / scale).min(img.width as f64);
            out.pixels[((oy + dy) * w + ox + dx) as usize] = area_average(img, sx0, sy0, sx1, sy1);
        }
    }
    out
}

fn area_average(img: &GrayImage, x0: f64, y0: f64, x1: f64, y1: f64) -> u8 {
    let (ix0, iy0) = (x0.floor() as u32, y0.floor() as u32);
    let (ix1, iy1) = ((x1.ceil() as u32).min(img.width).max(ix0 + 1), (y1.ceil() as u32).min(img.height).max(iy0 + 1));
    let (mut sum, mut weight) = (0.0, 0.0);
    for y in iy0..iy1 {
        let wy = (((y + 1) as f64).min(y1) - (y as f64).max(y0)).max(0.0);
        for x in ix0..ix1 {
            let wx = (((x + 1) as f64).min(x1) - (x as f64).max(x0)).max(0.0);
            let wgt = wx * wy;
            sum += img.pixels[(y * img.width + x) as usize] as f64 * wgt;
            weight += wgt;
        }
    }
    if weight <= 0.0 {
        255
    } else {
        (sum / weight).round().clamp(0.0, 255.0) as u8
    }
}

/// The 8x8 ordered-dither thresholds (0..63), the Bayer matrix. From Primitive Browser.
const BAYER8: [[u8; 8]; 8] = [
    [0, 32, 8, 40, 2, 34, 10, 42],
    [48, 16, 56, 24, 50, 18, 58, 26],
    [12, 44, 4, 36, 14, 46, 6, 38],
    [60, 28, 52, 20, 62, 30, 54, 22],
    [3, 35, 11, 43, 1, 33, 9, 41],
    [51, 19, 59, 27, 49, 17, 57, 25],
    [15, 47, 7, 39, 13, 45, 5, 37],
    [63, 31, 55, 23, 61, 29, 53, 21],
];

/// Ordered dither to the panel's 16 gray steps (multiples of 17), in place. The average gray of every
/// area is kept; flat areas get a fixed dot pattern, which stays calm on e-ink. From Primitive Browser.
pub fn dither16(img: &mut GrayImage) {
    let w = img.width as usize;
    for (i, px) in img.pixels.iter_mut().enumerate() {
        let (x, y) = (i % w, i / w);
        let base = (*px as u32) / 17;
        let frac = (*px as u32) % 17;
        let t = BAYER8[y % 8][x % 8] as u32;
        let up = frac * 128 > 17 * (2 * t + 1);
        *px = ((base + u32::from(up)) * 17).min(255) as u8;
    }
}

/// Draw the two panel steps next to black as black and the two next to white as white. Text and chrome
/// then change only pure pixels, which do not flash. From Primitive Browser.
pub fn solidify(gray: &mut [u8]) {
    for px in gray.iter_mut() {
        if *px <= 34 {
            *px = 0;
        } else if *px >= 221 {
            *px = 255;
        }
    }
}

/// A picture (sleep screen): decode, fit into `w` × `h`, dither.
pub fn picture_tile(png_bytes: &[u8], w: u32, h: u32) -> Result<GrayImage, String> {
    let img = decode_png(png_bytes)?;
    let mut tile = fit(&img, w, h);
    dither16(&mut tile);
    Ok(tile)
}

/// A Home Screen picture, centre-cropped to fill the frame and dithered to the panel's gray steps.
pub fn wallpaper_tile(png_bytes: &[u8], width: u32, height: u32) -> Result<GrayImage, String> {
    let source = decode_png(png_bytes)?;
    let mut tile = GrayImage::blank(width, height);
    if width == 0 || height == 0 {
        return Ok(tile);
    }
    let source_w = f64::from(source.width);
    let source_h = f64::from(source.height);
    let scale = (f64::from(width) / source_w).max(f64::from(height) / source_h);
    let left = ((source_w - f64::from(width) / scale) / 2.0).max(0.0);
    let top = ((source_h - f64::from(height) / scale) / 2.0).max(0.0);
    for y in 0..height {
        let y0 = top + f64::from(y) / scale;
        let y1 = (top + f64::from(y + 1) / scale).min(source_h);
        for x in 0..width {
            let x0 = left + f64::from(x) / scale;
            let x1 = (left + f64::from(x + 1) / scale).min(source_w);
            tile.pixels[(y * width + x) as usize] = area_average(&source, x0, y0, x1, y1);
        }
    }
    dither16(&mut tile);
    Ok(tile)
}

/// Corner radius of the rounded-square icons, as a share of their side (as on iOS).
pub const ICON_RADIUS: f32 = 0.225;
/// The icon outline's width in pixels: white icons keep their shape on the white page.
const ICON_OUTLINE: f32 = 3.0;

/// Signed distance from the point (`x`, `y`) to the edge of a rounded square of side `side` and corner radius
/// `radius` at the origin: negative inside.
pub fn rounded_distance(x: f32, y: f32, side: f32, radius: f32) -> f32 {
    let half = side / 2.0;
    let qx = (x - half).abs() - (half - radius);
    let qy = (y - half).abs() - (half - radius);
    let (ox, oy) = (qx.max(0.0), qy.max(0.0));
    (ox * ox + oy * oy).sqrt() + qx.max(qy).min(0.0) - radius
}

/// An app icon: the picture fitted into a rounded square of side `size` (white around a picture of another
/// shape), dithered, with a thin black outline and white outside the corners.
pub fn icon_tile(png_bytes: &[u8], size: u32) -> Result<GrayImage, String> {
    let img = decode_png(png_bytes)?;
    let mut tile = fit(&img, size, size);
    dither16(&mut tile);
    let (s, r) = (size as f32, size as f32 * ICON_RADIUS);
    for y in 0..size {
        for x in 0..size {
            let d = rounded_distance(x as f32 + 0.5, y as f32 + 0.5, s, r);
            let outer = (0.5 - d).clamp(0.0, 1.0);
            let inner = (0.5 - (d + ICON_OUTLINE)).clamp(0.0, 1.0);
            let px = &mut tile.pixels[(y * size + x) as usize];
            *px = (255.0 * (1.0 - outer) + *px as f32 * inner).round().clamp(0.0, 255.0) as u8;
        }
    }
    Ok(tile)
}

/// Darken the inside of a rounded-square icon at `r` in a screen buffer: the pressed look.
pub fn darken_icon(out: &mut [u8], width: usize, x0: i32, y0: i32, side: i32) {
    let height = (out.len() / width) as i32;
    let (s, r) = (side as f32, side as f32 * ICON_RADIUS);
    for y in y0.max(0)..(y0 + side).min(height) {
        for x in x0.max(0)..(x0 + side).min(width as i32) {
            if rounded_distance((x - x0) as f32 + 0.5, (y - y0) as f32 + 0.5, s, r) < 0.0 {
                let px = &mut out[y as usize * width + x as usize];
                *px = (*px as u32 * 45 / 100) as u8;
            }
        }
    }
}

/// A screen rectangle: x, y, width, height.
pub type Area = (i32, i32, i32, i32);

/// Signed distance from pixel (`x`, `y`) of a rectangle `w` × `h` with corner radius `radius` to its edge
/// (negative inside).
pub(super) fn rect_distance(x: i32, y: i32, w: i32, h: i32, radius: f32) -> f32 {
    let (hw, hh) = (w as f32 / 2.0, h as f32 / 2.0);
    let r = radius.min(hw).min(hh);
    let qx = (x as f32 + 0.5 - hw).abs() - (hw - r);
    let qy = (y as f32 + 0.5 - hh).abs() - (hh - r);
    (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - r
}

/// Draw a rounded card with a border of `border` pixels at `area` into a screen buffer: one step of the opening
/// zoom. White with a black border, or black with a white border in dark mode. Pure black and white, so the
/// panel can draw it with its fast waveform.
pub fn card(out: &mut [u8], width: usize, (x0, y0, w, h): Area, radius: f32, border: f32, dark: bool) {
    let height = (out.len() / width) as i32;
    let (edge, fill) = if dark { (255, 0) } else { (0, 255) };
    for y in y0.max(0)..(y0 + h).min(height) {
        for x in x0.max(0)..(x0 + w).min(width as i32) {
            let d = rect_distance(x - x0, y - y0, w, h, radius);
            if d < 0.0 {
                out[y as usize * width + x as usize] = if d > -border { edge } else { fill };
            }
        }
    }
}

/// Copy the rounded rectangle `area` (corner `radius`) of `layer` over `out`: a pop-up drawn in its own pass.
pub fn copy_rounded(out: &mut [u8], layer: &[u8], width: usize, (x0, y0, w, h): Area, radius: f32) {
    let height = (out.len().min(layer.len()) / width) as i32;
    for y in y0.max(0)..(y0 + h).min(height) {
        for x in x0.max(0)..(x0 + w).min(width as i32) {
            if rect_distance(x - x0, y - y0, w, h, radius) < 0.0 {
                let i = y as usize * width + x as usize;
                out[i] = layer[i];
            }
        }
    }
}

/// Dark mode: invert the screen, except the `pictures` (icons, the sleep picture), which keep their look inside
/// their rounded shape (`(area, corner radius)`; the corners outside it turn dark like the page). Where a pop-up
/// (`over`) covers a picture, the pop-up is inverted like the rest.
pub fn invert_except(out: &mut [u8], width: usize, pictures: &[(Area, f32)], over: Option<Area>) {
    for px in out.iter_mut() {
        *px = 255 - *px;
    }
    let height = (out.len() / width) as i32;
    let inside = |x: i32, y: i32, (ax, ay, aw, ah): Area| x >= ax && y >= ay && x < ax + aw && y < ay + ah;
    for &((x0, y0, w, h), radius) in pictures {
        for y in y0.max(0)..(y0 + h).min(height) {
            for x in x0.max(0)..(x0 + w).min(width as i32) {
                if !over.is_some_and(|o| inside(x, y, o)) && rect_distance(x - x0, y - y0, w, h, radius) < 0.0 {
                    let i = y as usize * width + x as usize;
                    out[i] = 255 - out[i];
                }
            }
        }
    }
}

/// Bumped when the icon tile's look changes, so cached tiles are made again.
const TILE_VERSION: &[u8] = b"rounded-square-1";

/// FNV-1a, for cache file names keyed by the icon's source bytes, the tile size and the tile version.
pub fn cache_key(bytes: &[u8], w: u32, h: u32) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in TILE_VERSION.iter().chain(bytes.iter()).chain(w.to_le_bytes().iter()).chain(h.to_le_bytes().iter()) {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

const CACHE_MAGIC: &[u8; 4] = b"KLG1";

/// Cache file: magic, width and height (u32 little endian), then the pixels.
pub fn to_cache(img: &GrayImage) -> Vec<u8> {
    let mut out = Vec::with_capacity(12 + img.pixels.len());
    out.extend_from_slice(CACHE_MAGIC);
    out.extend_from_slice(&img.width.to_le_bytes());
    out.extend_from_slice(&img.height.to_le_bytes());
    out.extend_from_slice(&img.pixels);
    out
}

pub fn from_cache(bytes: &[u8]) -> Option<GrayImage> {
    if bytes.len() < 12 || &bytes[..4] != CACHE_MAGIC {
        return None;
    }
    let width = u32::from_le_bytes(bytes[4..8].try_into().ok()?);
    let height = u32::from_le_bytes(bytes[8..12].try_into().ok()?);
    let pixels = bytes[12..].to_vec();
    (pixels.len() as u64 == width as u64 * height as u64).then_some(GrayImage { width, height, pixels })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_bytes(w: u32, h: u32, color: png::ColorType, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, w, h);
            enc.set_color(color);
            enc.set_depth(png::BitDepth::Eight);
            enc.write_header().unwrap().write_image_data(data).unwrap();
        }
        out
    }

    #[test]
    fn decodes_rgba_over_white_and_gray() {
        let rgba = png_bytes(2, 1, png::ColorType::Rgba, &[0, 0, 0, 255, 0, 0, 0, 0]);
        assert_eq!(decode_png(&rgba).unwrap().pixels, vec![0, 255]);
        let gray = png_bytes(1, 1, png::ColorType::Grayscale, &[100]);
        assert_eq!(decode_png(&gray).unwrap().pixels, vec![100]);
        assert!(decode_png(b"not a png").is_err());
    }

    #[test]
    fn fit_keeps_shape_and_averages() {
        // 4x2 black/white columns into 2x2: one row of two mid-grays, centred vertically on white.
        let img = GrayImage { width: 4, height: 2, pixels: vec![0, 255, 0, 255, 0, 255, 0, 255] };
        let out = fit(&img, 2, 2);
        assert_eq!(out.pixels, vec![128, 128, 255, 255]);
        let big = GrayImage { width: 600, height: 920, pixels: vec![0; 600 * 920] };
        let tile = fit(&big, 240, 360);
        assert_eq!((tile.width, tile.height), (240, 360));
        assert_eq!(tile.pixels[(180 * 240 + 120) as usize], 0);
    }

    #[test]
    fn wallpaper_crops_to_the_centre_without_stretching() {
        let stripes = [0, 85, 170, 255, 0, 85, 170, 255];
        let wide = png_bytes(4, 2, png::ColorType::Grayscale, &stripes);
        let cropped = wallpaper_tile(&wide, 2, 2).unwrap();
        assert_eq!((cropped.width, cropped.height), (2, 2));
        assert_eq!(cropped.pixels, [85, 170, 85, 170]);

        let tall = png_bytes(2, 4, png::ColorType::Grayscale, &[0, 0, 85, 85, 170, 170, 255, 255]);
        assert_eq!(wallpaper_tile(&tall, 2, 2).unwrap().pixels, [85, 85, 170, 170]);
    }

    #[test]
    fn dither_uses_panel_steps_and_keeps_the_average() {
        let mut img = GrayImage { width: 8, height: 8, pixels: vec![128; 64] };
        dither16(&mut img);
        assert!(img.pixels.iter().all(|p| p % 17 == 0));
        let avg = img.pixels.iter().map(|&p| p as u32).sum::<u32>() / 64;
        assert!((avg as i32 - 128).abs() <= 3, "average {avg}");
    }

    #[test]
    fn solidify_pushes_extremes() {
        let mut g = [0, 20, 34, 51, 204, 221, 250];
        solidify(&mut g);
        assert_eq!(g, [0, 0, 0, 51, 204, 255, 255]);
    }

    #[test]
    fn icon_tiles_are_rounded_squares_with_an_outline() {
        let gray = png_bytes(4, 4, png::ColorType::Grayscale, &[136; 16]);
        let tile = icon_tile(&gray, 100).unwrap();
        assert_eq!((tile.width, tile.height), (100, 100));
        let at = |x: u32, y: u32| tile.pixels[(y * 100 + x) as usize];
        assert_eq!(at(0, 0), 255, "outside the corner");
        assert_eq!(at(50, 1), 0, "the outline at the top edge");
        assert_eq!(at(50, 50), 136, "the picture inside");
    }

    #[test]
    fn darkening_stays_inside_the_rounded_square() {
        let mut out = vec![255u8; 20 * 20];
        darken_icon(&mut out, 20, 5, 5, 10);
        assert_eq!(out[0], 255);
        assert_eq!(out[5 * 20 + 5], 255, "the corner is outside");
        assert_eq!(out[10 * 20 + 10], 114);
        darken_icon(&mut out, 20, 15, 15, 10); // partly off the buffer: no panic
    }

    #[test]
    fn a_card_is_white_inside_a_black_border_with_round_corners() {
        let mut out = vec![128u8; 40 * 40];
        card(&mut out, 40, (5, 5, 30, 30), 8.0, 2.0, false);
        assert_eq!(out[20 * 40 + 20], 255, "inside");
        assert_eq!(out[20 * 40 + 5], 0, "left edge");
        assert_eq!(out[5 * 40 + 5], 128, "outside the round corner");
        assert_eq!(out[0], 128);
        assert!(out.iter().all(|&p| p == 0 || p == 255 || p == 128));
        card(&mut out, 40, (5, 5, 30, 30), 8.0, 2.0, true);
        assert_eq!((out[20 * 40 + 20], out[20 * 40 + 5]), (0, 255), "dark: black inside, white edge");
    }

    #[test]
    fn dark_mode_inverts_all_but_the_pictures_and_a_popup_covers_pictures() {
        let mut out = vec![200u8; 10 * 10];
        invert_except(&mut out, 10, &[((0, 0, 4, 4), 0.0)], Some((2, 2, 4, 4)));
        assert_eq!(out[0], 200, "a picture keeps its look");
        assert_eq!(out[3 * 10 + 3], 55, "under the pop-up it is inverted");
        assert_eq!(out[9 * 10 + 9], 55);
    }

    #[test]
    fn a_popup_is_copied_inside_its_rounded_corners() {
        let mut out = vec![0u8; 20 * 20];
        let layer = vec![255u8; 20 * 20];
        copy_rounded(&mut out, &layer, 20, (2, 2, 16, 16), 6.0);
        assert_eq!(out[10 * 20 + 10], 255);
        assert_eq!(out[2 * 20 + 2], 0, "the corner stays");
        assert_eq!(out[0], 0);
    }

    #[test]
    fn cache_round_trips_and_keys_differ_by_size() {
        let img = GrayImage { width: 2, height: 1, pixels: vec![1, 2] };
        assert_eq!(from_cache(&to_cache(&img)), Some(img));
        assert_eq!(from_cache(b"KLG1\x02\0\0\0\x02\0\0\0\x01"), None);
        assert_ne!(cache_key(b"a", 1, 2), cache_key(b"a", 2, 1));
    }
}
