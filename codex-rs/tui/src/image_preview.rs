//! Fullscreen previews for local conversation images, with an external viewer fallback.
//!
//! Native graphics share the transcript's placement and cleanup machinery. Terminals without
//! usable graphics geometry get a colored half-block preview, so opening an image never requires
//! a second terminal-input reader or an assumed Sixel cell size.

use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use image::DynamicImage;
use image::RgbaImage;
use image::imageops::FilterType;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Stylize;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use ratatui::widgets::Wrap;

use crate::key_hint::KeyBindingListExt;
use crate::keymap::PagerKeymap;
use crate::pets::ImageProtocol;
use crate::transcript_image::ImagePlacement;
use crate::transcript_image::TranscriptImage;
use crate::transcript_image::terminal_cell_size;
use crate::tui::Tui;
use crate::tui::TuiEvent;

const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_DECODED_BYTES: u64 = 64 * 1024 * 1024;

pub(crate) struct ImagePreviewOverlay {
    paths: Vec<PathBuf>,
    selected: usize,
    protocol: Option<ImageProtocol>,
    keymap: PagerKeymap,
    source: Option<Result<DynamicImage, String>>,
    preview: Option<PreparedPreview>,
    message: Option<String>,
    external_result: Option<mpsc::Receiver<Result<(), String>>>,
    is_done: bool,
}

struct PreparedPreview {
    bounds: (u16, u16),
    cell_size: Option<(u32, u32)>,
    image: PreviewImage,
}

enum PreviewImage {
    Native(Arc<TranscriptImage>),
    HalfBlocks(RgbaImage),
}

impl ImagePreviewOverlay {
    pub(crate) fn new(
        paths: Vec<PathBuf>,
        protocol: Option<ImageProtocol>,
        keymap: PagerKeymap,
    ) -> Self {
        Self {
            paths,
            selected: 0,
            protocol,
            keymap,
            source: None,
            preview: None,
            message: None,
            external_result: None,
            is_done: false,
        }
    }

    pub(crate) fn is_done(&self) -> bool {
        self.is_done
    }

    pub(crate) fn handle_event(&mut self, tui: &mut Tui, event: TuiEvent) -> io::Result<()> {
        #[cfg(windows)]
        if self.protocol == Some(ImageProtocol::Sixel)
            && matches!(&event, TuiEvent::Resize(_) | TuiEvent::Resume)
        {
            tui.probe_default_colors_after_protected_startup();
        }
        match event {
            TuiEvent::Key(key) if key.kind != KeyEventKind::Release => {
                self.handle_key(key);
                if !self.is_done && key.code == KeyCode::Char('o') && key.modifiers.is_empty() {
                    self.open_external(tui);
                }
                tui.frame_requester().schedule_frame();
            }
            TuiEvent::Draw | TuiEvent::Resume | TuiEvent::Resize(_) | TuiEvent::FocusGained => {
                self.poll_external_result();
                tui.draw(u16::MAX, |frame| {
                    if let Some(placement) = self.render(frame.area(), frame.buffer) {
                        frame.transcript_images.push(placement);
                    }
                })?;
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        if self.keymap.close.is_pressed(key)
            || key.code == KeyCode::Esc
            || (key.code == KeyCode::Char('q') && key.modifiers.is_empty())
            || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
        {
            self.is_done = true;
            return;
        }
        if !key.modifiers.is_empty() || self.paths.is_empty() {
            return;
        }
        let selected = match key.code {
            KeyCode::Left => self.selected.saturating_sub(1),
            KeyCode::Right => (self.selected + 1).min(self.paths.len() - 1),
            KeyCode::Home => 0,
            KeyCode::End => self.paths.len() - 1,
            _ => self.selected,
        };
        if selected != self.selected {
            self.selected = selected;
            self.source = None;
            self.preview = None;
            self.message = None;
            self.external_result = None;
        }
    }

    fn open_external(&mut self, tui: &Tui) {
        if self.external_result.is_some() {
            return;
        }
        let Some(path) = self.paths.get(self.selected).cloned() else {
            return;
        };
        let (sender, receiver) = mpsc::channel();
        let frame_requester = tui.frame_requester();
        std::thread::spawn(move || {
            let result = open_image_externally(&path).map_err(|error| error.to_string());
            let _ = sender.send(result);
            frame_requester.schedule_frame();
        });
        self.external_result = Some(receiver);
        self.message = Some("Opening external image viewer…".to_string());
    }

    fn poll_external_result(&mut self) {
        let Some(receiver) = &self.external_result else {
            return;
        };
        match receiver.try_recv() {
            Ok(result) => {
                self.message = Some(match result {
                    Ok(()) => "Opened in the external image viewer.".to_string(),
                    Err(error) => format!("Could not open the external image viewer: {error}"),
                });
                self.external_result = None;
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.message = Some("The external image viewer could not be started.".to_string());
                self.external_result = None;
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }

    fn render(&mut self, area: Rect, buffer: &mut Buffer) -> Option<ImagePlacement> {
        Clear.render(area, buffer);
        if area.is_empty() {
            return None;
        }
        let Some(path) = self.paths.get(self.selected) else {
            Paragraph::new("No local images to preview. Use /image <path> to open an image.")
                .wrap(Wrap { trim: false })
                .render(area, buffer);
            return None;
        };
        if self.source.is_none() {
            self.source = Some(load_image(path));
        }
        let name = path
            .file_name()
            .unwrap_or(path.as_os_str())
            .to_string_lossy()
            .replace(['\n', '\r', '\t'], " ");
        let dimensions = match self.source.as_ref() {
            Some(Ok(source)) => format!(" · {} × {}", source.width(), source.height()),
            _ => String::new(),
        };
        Paragraph::new(format!(
            "Image {}/{} · {name}{dimensions}",
            self.selected + 1,
            self.paths.len()
        ))
        .bold()
        .render(Rect::new(area.x, area.y, area.width, 1), buffer);

        let content = Rect::new(
            area.x.saturating_add(1),
            area.y.saturating_add(2),
            area.width.saturating_sub(2),
            area.height.saturating_sub(5),
        );
        let mut placement = None;
        let mut renderer = "Press o to open this image in an external viewer.";
        match self.source.as_ref() {
            Some(Ok(source)) if !content.is_empty() => {
                let cell_size = terminal_cell_size();
                if self.preview.as_ref().is_none_or(|preview| {
                    preview.bounds != (content.width, content.height)
                        || preview.cell_size != cell_size
                }) {
                    self.preview = Some(prepare_preview(source, content, self.protocol, cell_size));
                }
                if let Some(preview) = &self.preview {
                    match &preview.image {
                        PreviewImage::Native(image) => {
                            let image_area = centered(content, image.columns, image.rows);
                            placement = Some(ImagePlacement {
                                image: Arc::clone(image),
                                area: image_area,
                                first_row: 0,
                                file: None,
                            });
                            renderer = "Terminal graphics · o opens the full image externally.";
                        }
                        PreviewImage::HalfBlocks(pixels) => {
                            render_halfblocks(pixels, content, buffer);
                            renderer = "Color-block preview · o opens the full image externally.";
                        }
                    }
                }
            }
            Some(Err(error)) if !content.is_empty() => {
                Paragraph::new(format!("Cannot preview this image: {error}"))
                    .wrap(Wrap { trim: false })
                    .render(content, buffer);
            }
            _ => {}
        }
        if area.height >= 3 {
            Paragraph::new(self.message.as_deref().unwrap_or(renderer))
                .dim()
                .render(
                    Rect::new(area.x, area.bottom().saturating_sub(2), area.width, 1),
                    buffer,
                );
        }
        if area.height >= 2 {
            Paragraph::new("←/→ browse · o open externally · Esc/q close")
                .dim()
                .render(
                    Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1),
                    buffer,
                );
        }
        placement
    }
}

fn load_image(path: &Path) -> Result<DynamicImage, String> {
    let metadata = path.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() {
        return Err("the path is not a regular file".to_string());
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err(
            "the file exceeds the 32 MiB preview limit; press o to open it externally".to_string(),
        );
    }
    let mut reader = image::ImageReader::open(path)
        .and_then(image::ImageReader::with_guessed_format)
        .map_err(|error| error.to_string())?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16_384);
    limits.max_image_height = Some(16_384);
    limits.max_alloc = Some(MAX_DECODED_BYTES);
    reader.limits(limits);
    reader.decode().map_err(|error| error.to_string())
}

fn prepare_preview(
    source: &DynamicImage,
    area: Rect,
    protocol: Option<ImageProtocol>,
    cell_size: Option<(u32, u32)>,
) -> PreparedPreview {
    let cell_size = cell_size.filter(|(width, height)| *width > 0 && *height > 0);
    let native = match protocol {
        Some(ImageProtocol::Sixel) => cell_size.map(|size| (ImageProtocol::Sixel, size)),
        Some(protocol) => Some((protocol, cell_size.unwrap_or((8, 16)))),
        None => None,
    };
    let image = if let Some((protocol, (cell_width, cell_height))) = native {
        // The sprite Sixel encoder scans each active color per band. Keep photo previews bounded
        // as well as decoded input, including in unusually large terminal windows.
        let (max_width, max_height) = if protocol == ImageProtocol::Sixel {
            (640, 400)
        } else {
            (1_600, 1_000)
        };
        let width = u32::from(area.width)
            .saturating_mul(cell_width)
            .min(max_width)
            .max(1);
        let height = u32::from(area.height)
            .saturating_mul(cell_height)
            .min(max_height)
            .max(1);
        let mut pixels = source
            .resize(width, height, FilterType::Triangle)
            .to_rgba8();
        composite_checkerboard(&mut pixels, 8);
        PreviewImage::Native(Arc::new(TranscriptImage {
            protocol,
            columns: pixels.width().div_ceil(cell_width) as u16,
            rows: pixels.height().div_ceil(cell_height) as u16,
            pixels,
            cell_width,
            cell_height,
        }))
    } else {
        let mut pixels = source
            .resize(
                u32::from(area.width).clamp(1, 512),
                u32::from(area.height).saturating_mul(2).clamp(1, 1_024),
                FilterType::Triangle,
            )
            .to_rgba8();
        composite_checkerboard(&mut pixels, 4);
        PreviewImage::HalfBlocks(pixels)
    };
    PreparedPreview {
        bounds: (area.width, area.height),
        cell_size,
        image,
    }
}

fn composite_checkerboard(pixels: &mut RgbaImage, square_size: u32) {
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

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

fn render_halfblocks(pixels: &RgbaImage, area: Rect, buffer: &mut Buffer) {
    let image_area = centered(
        area,
        pixels.width() as u16,
        pixels.height().div_ceil(2) as u16,
    );
    for y in 0..image_area.height {
        for x in 0..image_area.width {
            let top = pixels.get_pixel(u32::from(x), u32::from(y) * 2);
            let bottom = pixels.get_pixel(
                u32::from(x),
                (u32::from(y) * 2 + 1).min(pixels.height() - 1),
            );
            buffer[(image_area.x + x, image_area.y + y)]
                .set_char('▀')
                .set_fg(Color::Rgb(top[0], top[1], top[2]))
                .set_bg(Color::Rgb(bottom[0], bottom[1], bottom[2]));
        }
    }
}

fn open_image_externally(path: &Path) -> io::Result<()> {
    // Keep ordinary Windows paths compatible with ShellExecuteW, and validate a symlink's
    // resolved filename as well as its contents before asking the OS to open it.
    let path = dunce::canonicalize(path)?;
    // A decoded image can have any suffix. Only dispatch known image filenames to the shell's
    // file association, never an executable or script merely containing decodable image data.
    if !is_image_filename(&path) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the file needs a recognized image extension to open externally",
        ));
    }
    if !path.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the image is not a regular file",
        ));
    }
    launch_image_viewer(&path)
}

fn is_image_filename(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "png"
                    | "jpg"
                    | "jpeg"
                    | "gif"
                    | "webp"
                    | "bmp"
                    | "tif"
                    | "tiff"
                    | "avif"
                    | "heic"
                    | "heif"
                    | "ico"
                    | "svg"
            )
        })
}

#[cfg(windows)]
fn launch_image_viewer(path: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::System::Com::COINIT_APARTMENTTHREADED;
    use windows_sys::Win32::System::Com::COINIT_DISABLE_OLE1DDE;
    use windows_sys::Win32::System::Com::CoInitializeEx;
    use windows_sys::Win32::System::Com::CoUninitialize;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    struct ComApartment;

    impl Drop for ComApartment {
        fn drop(&mut self) {
            // SAFETY: The guard is created only after successful initialization, remains on the
            // same worker thread, and balances both S_OK and S_FALSE results exactly once.
            unsafe { CoUninitialize() };
        }
    }

    // Shell file associations can invoke COM extension handlers that require an STA. This runs
    // on a dedicated viewer worker, independently of the TUI's apartment or event handling.
    // SAFETY: The reserved pointer is null and these are documented initialization flags.
    let result = unsafe {
        CoInitializeEx(
            std::ptr::null(),
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
        )
    };
    if result < 0 {
        return Err(io::Error::other(format!(
            "Windows image viewer COM initialization failed (HRESULT 0x{result:08X})"
        )));
    }
    let _com_apartment = ComApartment;

    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let operation: Vec<u16> = "open".encode_utf16().chain(Some(0)).collect();
    // SAFETY: All strings are NUL-terminated and live until the call returns. No command line
    // or shell interpolation is used; lpFile is the validated absolute image path.
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            operation.as_ptr(),
            path.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    } as isize;
    if result <= 32 {
        return Err(io::Error::other(format!(
            "Windows image association failed (code {result})"
        )));
    }
    Ok(())
}

#[cfg(not(windows))]
fn launch_image_viewer(path: &Path) -> io::Result<()> {
    use std::process::Command;
    use std::process::Stdio;

    let program = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let status = Command::new(program)
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if !status.success() {
        return Err(io::Error::other(format!("{program} exited with {status}")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key_hint::KeyBinding;
    use crate::keymap::RuntimeKeymap;
    use codex_config::types::KeybindingSpec;
    use codex_config::types::KeybindingsSpec;
    use codex_config::types::TuiKeymap;
    use image::Rgba;
    use pretty_assertions::assert_eq;

    fn image(width: u32, height: u32) -> DynamicImage {
        DynamicImage::ImageRgba8(RgbaImage::from_pixel(width, height, Rgba([255, 0, 0, 255])))
    }

    #[test]
    fn halfblocks_fit_preserve_aspect_and_use_two_pixels_per_cell() {
        let area = Rect::new(3, 4, 20, 10);
        let preview = prepare_preview(&image(100, 50), area, None, None);
        let PreviewImage::HalfBlocks(pixels) = preview.image else {
            panic!("expected half blocks")
        };
        assert_eq!(pixels.dimensions(), (20, 10));
        let mut buffer = Buffer::empty(area);
        render_halfblocks(&pixels, area, &mut buffer);
        assert_eq!(buffer[(3, 6)].symbol(), "▀");
        assert_eq!(buffer[(3, 6)].fg, Color::Rgb(255, 0, 0));
        assert_eq!(buffer[(3, 6)].bg, Color::Rgb(255, 0, 0));
        assert_eq!(buffer[(3, 5)].symbol(), " ");
    }

    #[test]
    fn sixel_requires_measured_cell_size_and_bounds_photographic_encoding() {
        let area = Rect::new(0, 0, 200, 100);
        let source = image(1_600, 1_000);
        let fallback = prepare_preview(&source, area, Some(ImageProtocol::Sixel), None);
        assert!(matches!(fallback.image, PreviewImage::HalfBlocks(_)));
        let native = prepare_preview(&source, area, Some(ImageProtocol::Sixel), Some((8, 16)));
        let PreviewImage::Native(native) = native.image else {
            panic!("expected Sixel")
        };
        assert_eq!(native.pixels.dimensions(), (640, 400));
        assert_eq!((native.columns, native.rows), (80, 25));
    }

    #[test]
    fn kitty_preview_fits_small_viewports() {
        let area = Rect::new(0, 0, 7, 3);
        let preview = prepare_preview(&image(50, 200), area, Some(ImageProtocol::Kitty), None);
        let PreviewImage::Native(native) = preview.image else {
            panic!("expected Kitty")
        };
        assert!(native.columns <= area.width);
        assert!(native.rows <= area.height);
        assert_eq!(native.rows, 3);
    }

    #[test]
    fn transparency_composites_without_losing_opaque_pixels() {
        let mut pixels = RgbaImage::from_pixel(9, 1, Rgba([255, 0, 0, 0]));
        pixels.put_pixel(1, 0, Rgba([255, 0, 0, 255]));
        pixels.put_pixel(2, 0, Rgba([255, 0, 0, 128]));
        composite_checkerboard(&mut pixels, 8);
        assert_eq!(*pixels.get_pixel(0, 0), Rgba([40, 40, 40, 255]));
        assert_eq!(*pixels.get_pixel(8, 0), Rgba([64, 64, 64, 255]));
        assert_eq!(*pixels.get_pixel(1, 0), Rgba([255, 0, 0, 255]));
        assert_eq!(*pixels.get_pixel(2, 0), Rgba([148, 20, 20, 255]));
    }

    #[test]
    fn navigation_invalidates_cached_image_and_stays_within_list() {
        let mut overlay = ImagePreviewOverlay::new(
            vec!["one.png".into(), "two.png".into()],
            None,
            RuntimeKeymap::defaults().pager,
        );
        overlay.source = Some(Ok(image(1, 1)));
        overlay.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(overlay.selected, 0);
        assert!(overlay.source.is_some());
        overlay.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(overlay.selected, 1);
        assert!(overlay.source.is_none());
        overlay.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(overlay.selected, 1);
        overlay.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(overlay.is_done());
    }

    #[test]
    fn configured_close_supports_remapped_keys_and_chord_dispatch_tokens() {
        let mut keymap = RuntimeKeymap::defaults().pager;
        keymap.close = vec![KeyBinding::new(KeyCode::Char('x'), KeyModifiers::ALT)];
        let mut overlay = ImagePreviewOverlay::new(Vec::new(), None, keymap);
        overlay.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(!overlay.is_done());
        overlay.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT));
        assert!(overlay.is_done());

        let mut config = TuiKeymap::default();
        config.pager.close = Some(KeybindingsSpec::One(KeybindingSpec(
            "ctrl-x ctrl-q".to_string(),
        )));
        let keymap = RuntimeKeymap::from_config(&config)
            .expect("configured close chord")
            .pager;
        let token = keymap
            .close
            .iter()
            .copied()
            .find(|binding| crate::keymap::is_dispatch_token(*binding))
            .expect("chord dispatch token");
        let (code, modifiers) = token.parts();
        let mut overlay = ImagePreviewOverlay::new(Vec::new(), None, keymap);
        overlay.handle_key(KeyEvent::new(code, modifiers));
        assert!(overlay.is_done());
    }

    #[test]
    fn decode_errors_and_empty_lists_render_without_panicking() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let path = temp.path().join("broken.png");
        std::fs::write(&path, "not an image").expect("write invalid image");
        assert!(load_image(&path).is_err());
        assert!(load_image(temp.path()).is_err());
        let mut overlay =
            ImagePreviewOverlay::new(vec![path], None, RuntimeKeymap::defaults().pager);
        let area = Rect::new(0, 0, 80, 20);
        let mut buffer = Buffer::empty(area);
        assert!(overlay.render(area, &mut buffer).is_none());
        let rendered: String = buffer.content().iter().map(|cell| cell.symbol()).collect();
        assert!(rendered.contains("Cannot preview this image"));
        let mut empty = ImagePreviewOverlay::new(Vec::new(), None, RuntimeKeymap::defaults().pager);
        assert!(empty.render(area, &mut buffer).is_none());
        assert!(empty.render(Rect::new(0, 0, 0, 0), &mut buffer).is_none());
    }

    #[test]
    fn external_viewer_does_not_dispatch_script_or_executable_filenames() {
        assert!(is_image_filename(Path::new("holiday photo.PNG")));
        assert!(is_image_filename(Path::new("-screenshot.webp")));
        assert!(!is_image_filename(Path::new("image.png.exe")));
        assert!(!is_image_filename(Path::new("image.bat")));
        assert!(!is_image_filename(Path::new("image")));
    }

    #[cfg(unix)]
    #[test]
    fn external_viewer_rejects_a_symlink_to_a_non_image_before_launch() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let target = temp.path().join("script.sh");
        let link = temp.path().join("image.png");
        std::fs::write(&target, "exit 1\n").expect("write target");
        std::os::unix::fs::symlink(&target, &link).expect("create symlink");
        assert_eq!(
            open_image_externally(&link)
                .expect_err("must reject script target")
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
