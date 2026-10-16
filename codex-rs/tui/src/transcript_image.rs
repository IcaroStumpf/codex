//! Bounded image previews for the owned fullscreen transcript.
//!
//! Only decoded local image pixels reach the terminal encoders. Previews are never source text;
//! their reserved rows belong to layout. The terminal retains visible placements and any local
//! transport files until those placements change.

use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use crossterm::cursor::MoveTo;
use crossterm::cursor::RestorePosition;
use crossterm::cursor::SavePosition;
use crossterm::queue;
use image::DynamicImage;
use image::ImageFormat;
use image::RgbaImage;
use image::imageops::FilterType;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

use crate::pets::ImageProtocol;

// Separate from the two image ids reserved for pets (0xC0DE and 0xC0DF).
pub(crate) const FIRST_IMAGE_ID: u32 = 0xC100;

pub(crate) struct ImagePlacement {
    pub(crate) image: Arc<TranscriptImage>,
    pub(crate) area: Rect,
    pub(crate) first_row: usize,
    pub(crate) file: Option<tempfile::NamedTempFile>,
}

impl PartialEq for ImagePlacement {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.image, &other.image)
            && self.area == other.area
            && self.first_row == other.first_row
    }
}

pub(crate) struct TranscriptImage {
    pub(crate) protocol: ImageProtocol,
    pub(crate) columns: u16,
    pub(crate) rows: u16,
    pub(crate) pixels: RgbaImage,
    pub(crate) cell_width: u32,
    pub(crate) cell_height: u32,
}

/// A preview painted as ordinary terminal cells, outside selectable transcript text.
pub(crate) struct HalfBlockImage {
    pub(crate) pixels: RgbaImage,
    pub(crate) columns: u16,
    pub(crate) rows: u16,
}

impl HalfBlockImage {
    pub(crate) fn load(path: &Path, width: u16) -> Option<Self> {
        let columns = width.saturating_sub(2).min(96);
        if columns == 0 {
            return None;
        }
        let source = decode_image(path)?;
        let pixels = halfblock_pixels(&source, Rect::new(0, 0, columns, 16), terminal_cell_size());
        Some(Self {
            columns: pixels.width() as u16,
            rows: pixels.height().div_ceil(2) as u16,
            pixels,
        })
    }

    pub(crate) fn render(&self, area: Rect, buffer: &mut Buffer, first_row: usize) {
        paint_halfblocks(&self.pixels, area, buffer, first_row);
    }
}

fn decode_image(path: &Path) -> Option<DynamicImage> {
    let metadata = path.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > 32 * 1024 * 1024 {
        return None;
    }
    let mut reader = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(32 * 1024 * 1024);
    reader.limits(limits);
    reader.decode().ok()
}

pub(crate) fn halfblock_pixels(
    source: &DynamicImage,
    area: Rect,
    cell_size: Option<(u32, u32)>,
) -> RgbaImage {
    let max_width = u32::from(area.width).clamp(1, 512);
    let max_height = u32::from(area.height).saturating_mul(2).clamp(1, 1_024);
    let (cell_width, cell_height) = cell_size
        .filter(|(width, height)| *width > 0 && *height > 0)
        .unwrap_or((1, 2));
    // Each sample occupies one cell's width and half its height.
    let aspect_width = u128::from(source.width().max(1)) * u128::from(cell_height);
    let aspect_height = u128::from(source.height().max(1)) * 2 * u128::from(cell_width);
    let (width, height) =
        if u128::from(max_width) * aspect_height <= u128::from(max_height) * aspect_width {
            (
                max_width,
                (u128::from(max_width) * aspect_height / aspect_width).max(1) as u32,
            )
        } else {
            (
                (u128::from(max_height) * aspect_width / aspect_height).max(1) as u32,
                max_height,
            )
        };
    let mut pixels = source
        .resize_exact(width, height, FilterType::Triangle)
        .to_rgba8();
    composite_checkerboard(&mut pixels, 4);
    pixels
}

pub(crate) fn composite_checkerboard(pixels: &mut RgbaImage, square_size: u32) {
    for (x, y, pixel) in pixels.enumerate_pixels_mut() {
        let background = if (x / square_size + y / square_size).is_multiple_of(2) {
            40
        } else {
            64
        };
        let alpha = u32::from(pixel[3]);
        for channel in &mut pixel.0[..3] {
            *channel =
                ((u32::from(*channel) * alpha + background * (255 - alpha) + 127) / 255) as u8;
        }
        pixel[3] = 255;
    }
}

// Image pixels retain their RGB colors independently of the terminal's UI theme.
#[allow(clippy::disallowed_methods)]
pub(crate) fn paint_halfblocks(
    pixels: &RgbaImage,
    area: Rect,
    buffer: &mut Buffer,
    first_row: usize,
) {
    let rows = (pixels.height().div_ceil(2) as usize).saturating_sub(first_row);
    for y in 0..usize::from(area.height).min(rows) {
        for x in 0..u32::from(area.width).min(pixels.width()) {
            let pixel_y = (first_row + y) as u32 * 2;
            let top = pixels.get_pixel(x, pixel_y);
            let bottom = pixels.get_pixel(x, (pixel_y + 1).min(pixels.height() - 1));
            buffer[(area.x + x as u16, area.y + y as u16)]
                .set_char('▀')
                .set_fg(Color::Rgb(top[0], top[1], top[2]))
                .set_bg(Color::Rgb(bottom[0], bottom[1], bottom[2]));
        }
    }
}

pub(crate) fn terminal_cell_size() -> Option<(u32, u32)> {
    crossterm::terminal::window_size()
        .ok()
        .filter(|size| size.columns > 0 && size.rows > 0 && size.width > 0 && size.height > 0)
        .map(|size| {
            (
                u32::from(size.width / size.columns).max(1),
                u32::from(size.height / size.rows).max(1),
            )
        })
        .or_else(|| {
            #[cfg(windows)]
            {
                crate::terminal_probe::cell_size()
            }
            #[cfg(not(windows))]
            {
                None
            }
        })
}

impl TranscriptImage {
    pub(crate) fn load(path: &Path, width: u16, protocol: ImageProtocol) -> Option<Self> {
        let columns = width.saturating_sub(2).min(48);
        if columns == 0 {
            return None;
        }
        // Sixel paints physical pixels rather than sizing to a Kitty cell rectangle. Guessing
        // the cell size could paint beyond the reserved rows and cover the composer.
        let (cell_width, cell_height) = match protocol {
            ImageProtocol::Sixel => terminal_cell_size()?,
            // Kitty scales to cell coordinates itself. Use a stable, typical cell aspect ratio.
            ImageProtocol::Kitty | ImageProtocol::KittyLocalFile => (8, 16),
        };
        let image = decode_image(path)?;
        // The sprite encoder scans each active color per Sixel band. Bound both its CPU
        // work and worst-case output even with large terminal cells or high-entropy images.
        let (max_width, max_height) = match protocol {
            ImageProtocol::Sixel => (160, 80),
            ImageProtocol::Kitty | ImageProtocol::KittyLocalFile => (384, 160),
        };
        let pixels = image
            .thumbnail(
                (u32::from(columns) * cell_width)
                    .min(image.width())
                    .min(max_width),
                (6 * cell_height).min(image.height()).min(max_height),
            )
            .to_rgba8();
        Some(Self {
            protocol,
            columns: pixels.width().div_ceil(cell_width) as u16,
            rows: pixels.height().div_ceil(cell_height) as u16,
            pixels,
            cell_width,
            cell_height,
        })
    }

    /// Draw only the visible vertical slice, never covering the prompt header or composer.
    pub(crate) fn draw(
        &self,
        writer: &mut impl Write,
        area: Rect,
        first_row: usize,
        id: u32,
    ) -> Result<Option<tempfile::NamedTempFile>> {
        let (top, bottom) = match self.protocol {
            ImageProtocol::Sixel => (
                first_row as u32 * self.cell_height,
                (first_row as u32 + u32::from(area.height)) * self.cell_height,
            ),
            // Kitty scales the entire raster to r=rows; clip at those scaled boundaries.
            ImageProtocol::Kitty | ImageProtocol::KittyLocalFile => (
                first_row as u32 * self.pixels.height() / u32::from(self.rows),
                (first_row as u32 + u32::from(area.height)) * self.pixels.height()
                    / u32::from(self.rows),
            ),
        };
        let top = top.min(self.pixels.height());
        let height = bottom.min(self.pixels.height()) - top;
        if height == 0 {
            return Ok(None);
        }
        let cropped =
            image::imageops::crop_imm(&self.pixels, 0, top, self.pixels.width(), height).to_image();
        let mut file = None;
        // Encoding and temp-file failures must not abort the TUI. Only writes to the actual
        // terminal below are fatal; the existing text entry remains usable without its preview.
        let payload = (|| -> Result<_> {
            Ok(match self.protocol {
                ImageProtocol::Kitty | ImageProtocol::KittyLocalFile => {
                    let mut png = tempfile::Builder::new().suffix(".png").tempfile()?;
                    DynamicImage::ImageRgba8(cropped).write_to(&mut png, ImageFormat::Png)?;
                    png.flush()?;
                    let transmit = if self.protocol == ImageProtocol::KittyLocalFile {
                        crate::pets::kitty_transmit_png_file_with_id
                    } else {
                        crate::pets::kitty_transmit_png_with_id
                    };
                    let command = transmit(png.path(), self.columns, area.height, Some(id))?;
                    file = (self.protocol == ImageProtocol::KittyLocalFile).then_some(png);
                    command.into_bytes()
                }
                ImageProtocol::Sixel => {
                    crate::pets::encode_rgba(cropped.as_raw(), cropped.width(), cropped.height())?
                }
            })
        })();
        let Ok(payload) = payload else {
            return Ok(None);
        };
        queue!(writer, SavePosition, MoveTo(area.x, area.y))?;
        writer.write_all(&payload)?;
        queue!(writer, RestorePosition)?;
        Ok(file)
    }
}
