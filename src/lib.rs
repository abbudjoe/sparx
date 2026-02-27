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
}

impl Default for RenderConfig {
    fn default() -> Self {
        Self {
            width: None,
            threshold: 128,
            color: true,
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

fn render_dynamic_image(
    image: &DynamicImage,
    config: &RenderConfig,
) -> Result<String, RenderError> {
    let (target_width_px, target_height_px) = target_dimensions(image, config.width);
    let resized = image
        .resize_exact(target_width_px, target_height_px, FilterType::Lanczos3)
        .to_rgba8();

    let mut output = String::new();
    let rows = target_height_px / 4;
    let cols = target_width_px / 2;

    for row in 0..rows {
        for col in 0..cols {
            let x = col * 2;
            let y = row * 4;
            let bits = braille_bits_rgba(&resized, x, y, config.threshold);
            if bits == 0 {
                output.push(' ');
                continue;
            }
            let ch = braille_char(bits)?;
            if config.color {
                let (r, g, b) = average_block_rgb_rgba(&resized, x, y, config.threshold);
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
    let raw_h = (aspect * target_width_px as f32 / 4.0).round() as u32;
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
        };
        render_image(bytes, &cfg).expect("render should succeed")
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
    fn invalid_bytes_returns_error() {
        let cfg = RenderConfig::default();
        let err = render_image(b"not an image", &cfg).expect_err("should error");
        assert!(matches!(err, RenderError::Image(_)));
    }

    #[test]
    fn terminal_width_detection_does_not_crash() {
        let _ = terminal_width();
    }
}
