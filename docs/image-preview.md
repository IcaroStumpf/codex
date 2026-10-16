# Terminal image previews

This fork adds a fullscreen image gallery to Codex CLI. It uses the existing
terminal graphics renderer and works with both Windows and Linux builds.

## Usage

- `/image` opens local images from the current conversation, most recent first.
  The gallery includes user attachments, images inspected with `view_image`, and
  saved image-generation results. Repeated paths appear once.
- `/image path/to/image.png` opens a local file relative to the current working
  directory. Absolute paths work too. Paths with spaces may be quoted; Windows
  backslashes are preserved.
- Left/Right switches between images.
- `o` opens the original file in the operating system's default image viewer.
- Esc or `q` closes the gallery and returns to the conversation.

The commands run locally without sending a prompt to the model. Images scale to
fit the available terminal area. Missing or unreadable files show an error in
the viewer without ending the Codex session.

## Rendering

The viewer uses Codex's existing Kitty or Sixel graphics support when available.
On Windows, the startup terminal probe requests the character-cell size needed
to position Sixel images. Unsupported terminals, terminal multiplexers, and
terminals that do not report the required geometry use a lower-resolution color
block preview. The external viewer action remains available for full detail.

PNG, JPEG, GIF, and WebP are supported by the existing image decoder. Animated
images are shown as a still frame. Remote image URLs are not downloaded. In a
remote-workspace session, use an explicit `/image <absolute-local-path>` to preview a file
on the computer running the CLI.

## Build and run this fork

From the repository's `codex-rs` directory, with the required Rust toolchain and
native build dependencies installed:

```shell
cargo build --bin codex
cargo run --bin codex
```

This runs the local build. The upstream npm package and official installers
install upstream releases and do not include this fork's changes. For general
build requirements, see [Installing & building](install.md).
