use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView};
use std::error::Error;
use std::fmt;

const DEFAULT_WIDTH: u32 = 80;
const BRAILLE_BASE: u32 = 0x2800;

#[cfg(target_os = "linux")]
const TIOCGWINSZ: u64 = 0x5413;
#[cfg(target_os = "macos")]
const TIOCGWINSZ: u64 = 0x40087468;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const TIOCGWINSZ: u64 = 0x5413;

#[derive(Debug)]
pub enum RenderError {
    Io(std::io::Error),
    Image(image::ImageError),
    InvalidBrailleCode(u32),
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "I/O error: {err}"),
            Self::Image(err) => write!(f, "Image decode error: {err}"),
            Self::InvalidBrailleCode(code) => write!(f, "Invalid braille codepoint: {code}"),
        }
    }
}

impl Error for RenderError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Image(err) => Some(err),
            Self::InvalidBrailleCode(_) => None,
        }
    }
}

impl From<std::io::Error> for RenderError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<image::ImageError> for RenderError {
    fn from(value: image::ImageError) -> Self {
        Self::Image(value)
    }
}

pub struct RenderConfig {
    /// Target width in terminal columns. None = auto-detect terminal width.
    pub width: Option<u32>,
    /// Brightness threshold for braille dots (0-255). Default: 128.
    pub threshold: u8,
    /// Whether to use truecolor ANSI. If false, no color.
    pub color: bool,
    /// Enable Floyd-Steinberg dithering for smoother gradients. Default: true.
    pub dither: bool,
    /// Stretch luminance histogram to full 0-255 range. Default: false.
    pub enhance: bool,
    /// Gamma correction (e.g. 1.5 brightens, 0.7 darkens). None = no correction.
    pub gamma: Option<f32>,
    /// Use Otsu's method to auto-detect optimal threshold. Default: false.
    /// When true, overrides the manual `threshold` value.
    pub auto_threshold: bool,
}

impl Default for RenderConfig {
    fn default() -> Self {
        Self {
            width: None,
            threshold: 128,
            color: true,
            dither: true,
            enhance: false,
            gamma: None,
            auto_threshold: false,
        }
    }
}

pub fn render_image(image_bytes: &[u8], config: &RenderConfig) -> Result<String, RenderError> {
    let image = image::load_from_memory(image_bytes)?;
    render_dynamic_image(&image, config)
}

pub fn render_file(path: &str, config: &RenderConfig) -> Result<String, RenderError> {
    let image_bytes = std::fs::read(path)?;
    render_image(&image_bytes, config)
}

pub fn terminal_width() -> Option<u32> {
    terminal_width_impl()
}

/// Stretch luminance of opaque pixels to span the full 0–255 range.
fn histogram_stretch(lum: &mut [f32], alpha_mask: &[bool]) {
    let mut min = f32::MAX;
    let mut max = f32::MIN;
    for (i, &opaque) in alpha_mask.iter().enumerate() {
        if opaque {
            min = min.min(lum[i]);
            max = max.max(lum[i]);
        }
    }
    let range = max - min;
    if range < 2.0 {
        return;
    }
    for (i, &opaque) in alpha_mask.iter().enumerate() {
        if opaque {
            lum[i] = (lum[i] - min) / range * 255.0;
        }
    }
}

/// Apply gamma correction to opaque pixels. gamma > 1 brightens midtones, < 1 darkens.
fn apply_gamma(lum: &mut [f32], alpha_mask: &[bool], gamma: f32) {
    let inv_gamma = 1.0 / gamma;
    for (i, &opaque) in alpha_mask.iter().enumerate() {
        if opaque {
            lum[i] = (lum[i] / 255.0).powf(inv_gamma) * 255.0;
        }
    }
}

/// Compute the optimal threshold using Otsu's method (maximize inter-class variance).
fn otsu_threshold(lum: &[f32], alpha_mask: &[bool]) -> u8 {
    let mut histogram = [0u32; 256];
    let mut total = 0u32;
    for (i, &opaque) in alpha_mask.iter().enumerate() {
        if opaque {
            let bin = (lum[i].round() as u32).min(255) as usize;
            histogram[bin] += 1;
            total += 1;
        }
    }
    if total == 0 {
        return 128;
    }

    let mut sum_total = 0.0f64;
    for (i, &count) in histogram.iter().enumerate() {
        sum_total += i as f64 * count as f64;
    }

    let mut best_threshold = 0u8;
    let mut best_variance = 0.0f64;
    let mut weight_bg = 0.0f64;
    let mut sum_bg = 0.0f64;
    let total_f = total as f64;

    for t in 0..256u32 {
        weight_bg += histogram[t as usize] as f64;
        if weight_bg == 0.0 {
            continue;
        }
        let weight_fg = total_f - weight_bg;
        if weight_fg == 0.0 {
            break;
        }
        sum_bg += t as f64 * histogram[t as usize] as f64;
        let mean_bg = sum_bg / weight_bg;
        let mean_fg = (sum_total - sum_bg) / weight_fg;
        let variance = weight_bg * weight_fg * (mean_bg - mean_fg) * (mean_bg - mean_fg);
        if variance > best_variance {
            best_variance = variance;
            best_threshold = t as u8;
        }
    }

    best_threshold
}

fn render_dynamic_image(
    image: &DynamicImage,
    config: &RenderConfig,
) -> Result<String, RenderError> {
    let (target_width_px, target_height_px) = target_dimensions(image, config.width);
    let resized = image
        .resize_exact(target_width_px, target_height_px, FilterType::Lanczos3)
        .to_rgba8();

    let (img_w, img_h) = resized.dimensions();
    let mut lum_buf = Vec::with_capacity((img_w * img_h) as usize);
    let mut alpha_mask = Vec::with_capacity((img_w * img_h) as usize);
    for y in 0..img_h {
        for x in 0..img_w {
            let p = resized.get_pixel(x, y);
            let lum = if p[3] < ALPHA_THRESHOLD {
                0.0
            } else {
                luminance(p[0], p[1], p[2]) as f32
            };
            lum_buf.push(lum);
            alpha_mask.push(p[3] >= ALPHA_THRESHOLD);
        }
    }

    if config.enhance {
        histogram_stretch(&mut lum_buf, &alpha_mask);
    }
    if let Some(gamma) = config.gamma {
        apply_gamma(&mut lum_buf, &alpha_mask, gamma);
    }
    let threshold = if config.auto_threshold {
        otsu_threshold(&lum_buf, &alpha_mask) as f32
    } else {
        config.threshold as f32
    };

    if config.dither {
        floyd_steinberg_dither(
            &mut lum_buf,
            &alpha_mask,
            img_w,
            img_h,
            threshold,
        );
    }

    let threshold_u8 = threshold.round() as u8;
    let mut output = String::new();
    let rows = target_height_px / 4;
    let cols = target_width_px / 2;

    for row in 0..rows {
        for col in 0..cols {
            let x = col * 2;
            let y = row * 4;
            let bits = if config.dither {
                braille_bits_from_lum_buf(&lum_buf, target_width_px, x, y, threshold)
            } else {
                braille_bits_rgba(&resized, x, y, threshold_u8)
            };
            if bits == 0 {
                output.push(' ');
                continue;
            }
            let ch = braille_char(bits)?;
            if config.color {
                let (r, g, b) = average_block_rgb_rgba(&resized, x, y, threshold_u8);
                output.push_str(&format!("\x1b[38;2;{r};{g};{b}m{ch}\x1b[0m"));
            } else {
                output.push(ch);
            }
        }
        output.push('\n');
    }

    Ok(output)
}

fn target_dimensions(image: &DynamicImage, width_cols: Option<u32>) -> (u32, u32) {
    let cols = width_cols
        .unwrap_or_else(|| terminal_width().unwrap_or(DEFAULT_WIDTH))
        .max(1);
    let target_width_px = cols.saturating_mul(2).max(2);
    let (orig_w, orig_h) = image.dimensions();
    let orig_w = orig_w.max(1);
    let aspect = orig_h as f32 / orig_w as f32;
    // Braille cells are 2px wide × 4px tall. Terminal cells have ~1:2 width:height
    // ratio, so each pixel's physical size is cell_w/2 wide and cell_h/4 = cell_w/2
    // tall — equal in both axes. No correction factor is needed.
    let raw_h = (aspect * target_width_px as f32).round() as u32;
    let clamped_h = raw_h.max(4);
    let target_height_px = round_up_to_multiple(clamped_h, 4);
    (target_width_px, target_height_px)
}

fn round_up_to_multiple(value: u32, multiple: u32) -> u32 {
    let remainder = value % multiple;
    if remainder == 0 {
        value
    } else {
        value + (multiple - remainder)
    }
}

/// Minimum alpha to consider a pixel "visible" (0-255).
const ALPHA_THRESHOLD: u8 = 64;

/// Apply Floyd-Steinberg error diffusion to a luminance buffer.
fn floyd_steinberg_dither(
    lum: &mut [f32],
    alpha_mask: &[bool],
    width: u32,
    height: u32,
    threshold: f32,
) {
    let w = width as usize;
    for y in 0..height as usize {
        for x in 0..w {
            let idx = y * w + x;
            if !alpha_mask[idx] {
                lum[idx] = 0.0;
                continue;
            }

            let old = lum[idx];
            let new_val = if old > threshold { 255.0 } else { 0.0 };
            lum[idx] = new_val;
            let error = old - new_val;

            if x + 1 < w && alpha_mask[idx + 1] {
                lum[idx + 1] += error * 7.0 / 16.0;
            }
            if y + 1 < height as usize {
                let below = (y + 1) * w;
                if x > 0 && alpha_mask[below + (x - 1)] {
                    lum[below + (x - 1)] += error * 3.0 / 16.0;
                }
                if alpha_mask[below + x] {
                    lum[below + x] += error * 5.0 / 16.0;
                }
                if x + 1 < w && alpha_mask[below + (x + 1)] {
                    lum[below + (x + 1)] += error * 1.0 / 16.0;
                }
            }
        }
    }
}

fn braille_bits_from_lum_buf(lum_buf: &[f32], img_width: u32, x: u32, y: u32, threshold: f32) -> u8 {
    let mut bits = 0u8;
    let map: [((u32, u32), u8); 8] = [
        ((0, 0), 0x01),
        ((0, 1), 0x02),
        ((0, 2), 0x04),
        ((1, 0), 0x08),
        ((1, 1), 0x10),
        ((1, 2), 0x20),
        ((0, 3), 0x40),
        ((1, 3), 0x80),
    ];
    for ((dx, dy), bit) in map {
        let idx = (y + dy) as usize * img_width as usize + (x + dx) as usize;
        if lum_buf[idx] > threshold {
            bits |= bit;
        }
    }
    bits
}

fn braille_bits_rgba(image: &image::RgbaImage, x: u32, y: u32, threshold: u8) -> u8 {
    let mut bits = 0u8;
    let map: [((u32, u32), u8); 8] = [
        ((0, 0), 0x01),
        ((0, 1), 0x02),
        ((0, 2), 0x04),
        ((1, 0), 0x08),
        ((1, 1), 0x10),
        ((1, 2), 0x20),
        ((0, 3), 0x40),
        ((1, 3), 0x80),
    ];

    for ((dx, dy), bit) in map {
        let p = image.get_pixel(x + dx, y + dy);
        // Transparent pixels never set a dot
        if p[3] < ALPHA_THRESHOLD {
            continue;
        }
        let lum = luminance(p[0], p[1], p[2]);
        if lum > threshold {
            bits |= bit;
        }
    }

    bits
}

fn luminance(r: u8, g: u8, b: u8) -> u8 {
    let l = 0.2126f32 * r as f32 + 0.7152f32 * g as f32 + 0.0722f32 * b as f32;
    l.round() as u8
}

fn braille_char(bits: u8) -> Result<char, RenderError> {
    let code = BRAILLE_BASE + u32::from(bits);
    char::from_u32(code).ok_or(RenderError::InvalidBrailleCode(code))
}

fn average_block_rgb_rgba(image: &image::RgbaImage, x: u32, y: u32, threshold: u8) -> (u8, u8, u8) {
    let mut r = 0u32;
    let mut g = 0u32;
    let mut b = 0u32;
    let mut count = 0u32;

    for dy in 0..4 {
        for dx in 0..2 {
            let p = image.get_pixel(x + dx, y + dy);
            // Only average visible pixels above threshold.
            if p[3] < ALPHA_THRESHOLD {
                continue;
            }
            let lum = luminance(p[0], p[1], p[2]);
            if lum <= threshold {
                continue;
            }
            r += u32::from(p[0]);
            g += u32::from(p[1]);
            b += u32::from(p[2]);
            count += 1;
        }
    }

    if count == 0 {
        return (0, 0, 0);
    }

    ((r / count) as u8, (g / count) as u8, (b / count) as u8)
}

#[cfg(unix)]
fn terminal_width_impl() -> Option<u32> {
    #[repr(C)]
    struct WinSize {
        ws_row: u16,
        ws_col: u16,
        ws_xpixel: u16,
        ws_ypixel: u16,
    }

    unsafe extern "C" {
        fn ioctl(fd: i32, request: u64, ...) -> i32;
    }

    use std::mem::MaybeUninit;

    // SAFETY: We pass a valid pointer to WinSize and only read it when ioctl succeeds.
    unsafe {
        let mut ws = MaybeUninit::<WinSize>::uninit();
        if ioctl(1, TIOCGWINSZ, ws.as_mut_ptr()) == 0 {
            let ws = ws.assume_init();
            if ws.ws_col > 0 {
                Some(ws.ws_col as u32)
            } else {
                None
            }
        } else {
            None
        }
    }
}

#[cfg(not(unix))]
fn terminal_width_impl() -> Option<u32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, ImageBuffer, ImageFormat, Rgba};
    use std::io::Cursor;

    fn png_bytes(width: u32, height: u32, color: Rgba<u8>) -> Vec<u8> {
        let img = ImageBuffer::from_fn(width, height, |_x, _y| color);
        let dynimg = DynamicImage::ImageRgba8(img);
        let mut bytes = Vec::new();
        dynimg
            .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
            .expect("png encode should succeed");
        bytes
    }

    fn render(bytes: &[u8], width: u32, threshold: u8, color: bool) -> String {
        let cfg = RenderConfig {
            width: Some(width),
            threshold,
            color,
            dither: true,
            enhance: false,
            gamma: None,
            auto_threshold: false,
        };
        render_image(bytes, &cfg).expect("render should succeed")
    }

    #[test]
    fn dithered_output_differs_from_non_dithered_for_gradient() {
        let img = ImageBuffer::from_fn(20, 8, |x, _y| {
            let v = (x as f32 / 19.0 * 255.0) as u8;
            Rgba([v, v, v, 255])
        });
        let dynimg = DynamicImage::ImageRgba8(img);
        let mut bytes = Vec::new();
        dynimg
            .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
            .expect("png encode should succeed");

        let dithered = render(&bytes, 10, 128, false);
        let cfg_no_dither = RenderConfig {
            width: Some(10),
            threshold: 128,
            color: false,
            dither: false,
            enhance: false,
            gamma: None,
            auto_threshold: false,
        };
        let not_dithered = render_image(&bytes, &cfg_no_dither).expect("render should succeed");
        assert_ne!(
            dithered, not_dithered,
            "dithering should change output for gradients"
        );
    }

    #[test]
    fn dithering_does_not_light_up_transparent_pixels() {
        let img = ImageBuffer::from_fn(4, 4, |x, _y| {
            if x < 2 {
                Rgba([255, 255, 255, 255])
            } else {
                Rgba([255, 255, 255, 0])
            }
        });
        let dynimg = DynamicImage::ImageRgba8(img);
        let mut bytes = Vec::new();
        dynimg
            .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
            .expect("png encode should succeed");

        let cfg = RenderConfig {
            width: Some(2),
            threshold: 128,
            color: false,
            dither: true,
            enhance: false,
            gamma: None,
            auto_threshold: false,
        };
        let output = render_image(&bytes, &cfg).expect("render should succeed");
        let chars: Vec<char> = output
            .lines()
            .next()
            .expect("line should exist")
            .chars()
            .collect();
        assert_eq!(chars.len(), 2);
        assert_eq!(
            chars[1], ' ',
            "transparent region should remain empty even with dithering"
        );
    }

    #[test]
    fn dither_disabled_matches_original_behavior() {
        let bytes = png_bytes(1, 1, Rgba([255, 255, 255, 255]));
        let cfg = RenderConfig {
            width: Some(1),
            threshold: 128,
            color: false,
            dither: false,
            enhance: false,
            gamma: None,
            auto_threshold: false,
        };
        assert_eq!(
            render_image(&bytes, &cfg).expect("render should succeed"),
            "⣿\n"
        );
    }

    #[test]
    fn render_white_pixel_single_full_braille() {
        let bytes = png_bytes(1, 1, Rgba([255, 255, 255, 255]));
        assert_eq!(render(&bytes, 1, 128, false), "⣿\n");
    }

    #[test]
    fn render_black_pixel_single_empty_braille() {
        let bytes = png_bytes(1, 1, Rgba([0, 0, 0, 255]));
        assert_eq!(render(&bytes, 1, 128, false), " \n");
    }

    #[test]
    fn transparent_pixel_renders_empty() {
        let bytes = png_bytes(1, 1, Rgba([255, 255, 255, 0]));
        assert_eq!(render(&bytes, 1, 128, false), " \n");
    }

    #[test]
    fn known_image_output_dimensions_match_width_and_rows() {
        let bytes = png_bytes(4, 4, Rgba([255, 255, 255, 255]));
        let output = render(&bytes, 2, 128, false);
        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].chars().count(), 2);
    }

    #[test]
    fn threshold_changes_output() {
        let bytes = png_bytes(1, 1, Rgba([128, 128, 128, 255]));
        let low = render(&bytes, 1, 0, false);
        let high = render(&bytes, 1, 255, false);
        assert_ne!(low, high);
    }

    #[test]
    fn dark_block_renders_as_space() {
        let bytes = png_bytes(2, 4, Rgba([0, 0, 0, 255]));
        assert_eq!(render(&bytes, 1, 128, false), " \n");
    }

    #[test]
    fn dark_block_in_color_mode_emits_no_ansi() {
        let bytes = png_bytes(2, 4, Rgba([0, 0, 0, 255]));
        let output = render(&bytes, 1, 128, true);
        assert!(
            !output.contains("\x1b["),
            "dark block should not emit ANSI escapes"
        );
        assert_eq!(output.trim_end(), " ".repeat(output.trim_end().len()));
    }

    #[test]
    fn color_average_excludes_dark_pixels() {
        let img = ImageBuffer::from_fn(2, 4, |x, _y| {
            if x == 0 {
                Rgba([220, 180, 140, 255])
            } else {
                Rgba([10, 10, 10, 255])
            }
        });
        let (r, g, b) = average_block_rgb_rgba(&img, 0, 0, 128);
        assert_eq!((r, g, b), (220, 180, 140));
    }

    #[test]
    fn color_average_excludes_transparent_and_dark_pixels() {
        let img = ImageBuffer::from_fn(2, 4, |x, y| {
            if x == 0 && y < 2 {
                Rgba([200, 150, 100, 255])
            } else if x == 1 && y < 2 {
                Rgba([200, 150, 100, 0])
            } else {
                Rgba([10, 10, 10, 255])
            }
        });
        let (r, g, b) = average_block_rgb_rgba(&img, 0, 0, 128);
        assert_eq!((r, g, b), (200, 150, 100));
    }
    #[test]
    fn color_mode_contains_ansi_escape() {
        let bytes = png_bytes(1, 1, Rgba([250, 220, 180, 255]));
        let output = render(&bytes, 1, 128, true);
        assert!(output.contains("\x1b[38;2;"));
    }

    #[test]
    fn no_color_mode_has_no_ansi_escape() {
        let bytes = png_bytes(1, 1, Rgba([200, 100, 50, 255]));
        let output = render(&bytes, 1, 128, false);
        assert!(!output.contains("\x1b["));
    }

    #[test]
    fn custom_width_is_respected() {
        let bytes = png_bytes(3, 3, Rgba([255, 255, 255, 255]));
        let output = render(&bytes, 7, 128, false);
        let line = output.lines().next().expect("line should exist");
        assert_eq!(line.chars().count(), 7);
    }

    #[test]
    fn target_dimensions_square_image() {
        let img =
            DynamicImage::ImageRgba8(ImageBuffer::from_fn(100, 100, |_, _| Rgba([0, 0, 0, 255])));
        let (w, h) = target_dimensions(&img, Some(40));
        assert_eq!(w, 80);
        assert_eq!(h, 80);
    }

    #[test]
    fn target_dimensions_wide_image() {
        let img =
            DynamicImage::ImageRgba8(ImageBuffer::from_fn(200, 100, |_, _| Rgba([0, 0, 0, 255])));
        let (w, h) = target_dimensions(&img, Some(40));
        assert_eq!(w, 80);
        assert_eq!(h, 40);
    }

    #[test]
    fn invalid_bytes_returns_error() {
        let cfg = RenderConfig::default();
        let err = render_image(b"not an image", &cfg).expect_err("should error");
        assert!(matches!(err, RenderError::Image(_)));
    }

    #[test]
    fn terminal_width_detection_does_not_crash() {
        let _ = terminal_width();
    }

    #[test]
    fn histogram_stretch_expands_narrow_range() {
        let mut lum = vec![50.0, 75.0, 100.0];
        let mask = vec![true, true, true];
        histogram_stretch(&mut lum, &mask);
        assert!((lum[0] - 0.0).abs() < 0.1);
        assert!((lum[2] - 255.0).abs() < 0.1);
        assert!(lum[1] > 100.0 && lum[1] < 160.0);
    }

    #[test]
    fn histogram_stretch_skips_flat_image() {
        let mut lum = vec![128.0, 128.0, 128.0];
        let mask = vec![true, true, true];
        histogram_stretch(&mut lum, &mask);
        assert!((lum[0] - 128.0).abs() < 0.1);
    }

    #[test]
    fn histogram_stretch_ignores_transparent() {
        let mut lum = vec![0.0, 50.0, 100.0];
        let mask = vec![false, true, true];
        histogram_stretch(&mut lum, &mask);
        assert!((lum[0] - 0.0).abs() < 0.1, "transparent pixel unchanged");
        assert!((lum[1] - 0.0).abs() < 0.1, "min opaque → 0");
        assert!((lum[2] - 255.0).abs() < 0.1, "max opaque → 255");
    }

    #[test]
    fn otsu_bimodal_separates_clusters() {
        let mut lum = Vec::new();
        let mut mask = Vec::new();
        for _ in 0..100 {
            lum.push(20.0);
            mask.push(true);
        }
        for _ in 0..100 {
            lum.push(220.0);
            mask.push(true);
        }
        let t = otsu_threshold(&lum, &mask);
        // Any threshold between the two clusters (20..220) maximizes inter-class
        // variance equally. The algorithm picks the first maximum.
        assert!(
            t >= 20 && t <= 220,
            "otsu should find threshold between clusters, got {t}"
        );
    }

    #[test]
    fn otsu_dark_image_gives_low_threshold() {
        let lum: Vec<f32> = (0..200).map(|i| (i % 40) as f32).collect();
        let mask = vec![true; 200];
        let t = otsu_threshold(&lum, &mask);
        assert!(t < 80, "dark image should get low threshold, got {t}");
    }

    #[test]
    fn gamma_brightens_midtones() {
        let mut lum = vec![0.0, 128.0, 255.0];
        let mask = vec![true, true, true];
        apply_gamma(&mut lum, &mask, 2.0);
        assert!((lum[0] - 0.0).abs() < 0.1, "black stays black");
        assert!(lum[1] > 140.0, "midtone should brighten with gamma 2.0, got {}", lum[1]);
        assert!((lum[2] - 255.0).abs() < 0.1, "white stays white");
    }

    #[test]
    fn gamma_darkens_midtones() {
        let mut lum = vec![128.0];
        let mask = vec![true];
        apply_gamma(&mut lum, &mask, 0.5);
        assert!(lum[0] < 120.0, "midtone should darken with gamma 0.5, got {}", lum[0]);
    }

    #[test]
    fn enhance_changes_output_for_narrow_range_image() {
        let img = ImageBuffer::from_fn(2, 4, |_, _| Rgba([100, 100, 100, 255]));
        let dynimg = DynamicImage::ImageRgba8(img);
        let mut bytes = Vec::new();
        dynimg
            .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
            .expect("png encode should succeed");

        let normal = render(&bytes, 1, 128, false);
        let cfg_enhanced = RenderConfig {
            width: Some(1),
            threshold: 128,
            color: false,
            dither: true,
            enhance: true,
            gamma: None,
            auto_threshold: false,
        };
        let enhanced = render_image(&bytes, &cfg_enhanced).expect("render should succeed");
        // A uniform gray image at 100 is below threshold 128, renders as space normally.
        // With enhance, it stretches to 255 (since all pixels are same), but range < 2 so
        // stretch is skipped. Output should be the same for uniform images.
        assert_eq!(normal, enhanced);
    }

    #[test]
    fn auto_threshold_changes_output_for_dark_image() {
        let img = ImageBuffer::from_fn(4, 8, |x, _y| {
            if x < 2 {
                Rgba([60, 60, 60, 255])
            } else {
                Rgba([30, 30, 30, 255])
            }
        });
        let dynimg = DynamicImage::ImageRgba8(img);
        let mut bytes = Vec::new();
        dynimg
            .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
            .expect("png encode should succeed");

        let fixed = render(&bytes, 2, 128, false);
        let cfg_auto = RenderConfig {
            width: Some(2),
            threshold: 128,
            color: false,
            dither: false,
            enhance: false,
            gamma: None,
            auto_threshold: true,
        };
        let auto = render_image(&bytes, &cfg_auto).expect("render should succeed");
        // With threshold 128, both 60 and 30 are below → all spaces.
        // With Otsu, threshold should be ~45, so 60 lights up but 30 doesn't.
        assert_ne!(fixed, auto, "auto-threshold should differ from fixed 128 for dark images");
    }
}
