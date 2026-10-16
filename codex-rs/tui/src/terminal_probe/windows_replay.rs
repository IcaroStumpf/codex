//! Identify terminal replies without changing unrelated Windows console input.

use std::ops::Range;

const MAX_TERMINAL_COLOR_RESPONSE_BYTES: usize = 1_024;
const MAX_TERMINAL_CELL_RESPONSE_BYTES: usize = 32;
const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

/// Return byte ranges occupied by complete, valid color and cell-size responses.
///
/// The Windows probe maps these byte ranges back to their original `INPUT_RECORD`s so keyboard
/// modifiers, UTF-16 input, mouse events, and focus changes can be replayed without reconstruction.
pub(super) fn terminal_response_ranges(input: &[u8]) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut cursor = 0;

    while cursor < input.len() {
        if input[cursor..].starts_with(PASTE_START) {
            let payload_start = cursor + PASTE_START.len();
            let Some(payload_end) = super::find_subslice(&input[payload_start..], PASTE_END) else {
                break;
            };
            cursor = payload_start + payload_end + PASTE_END.len();
            continue;
        }
        if input[cursor..].starts_with(b"\x1b[6;") {
            let response = &input[cursor..];
            if let Some(end) = response
                .iter()
                .take(MAX_TERMINAL_CELL_RESPONSE_BYTES)
                .position(|byte| *byte == b't')
                && parse_cell_size_response(&response[..=end]).is_some()
            {
                ranges.push(cursor..cursor + end + 1);
                cursor += end + 1;
                continue;
            }
        }
        if !input[cursor..].starts_with(b"\x1b]") {
            cursor += 1;
            continue;
        }

        let start = cursor;
        let response = &input[start..];
        let Some((slot, prefix)) = [(10_u8, b"\x1b]10;"), (11_u8, b"\x1b]11;")]
            .into_iter()
            .find(|(_, prefix)| response.starts_with(*prefix))
        else {
            cursor = start + b"\x1b]".len();
            continue;
        };

        let payload_start = start + prefix.len();
        let bounded_end = input
            .len()
            .min(start.saturating_add(MAX_TERMINAL_COLOR_RESPONSE_BYTES));
        let Some((payload_len, terminator_len)) =
            super::osc_payload_end(&input[payload_start..bounded_end])
        else {
            cursor = payload_start;
            continue;
        };
        let end = payload_start + payload_len + terminator_len;
        if super::parse_osc_color(&input[start..end], slot).is_some() {
            ranges.push(start..end);
        }
        cursor = end;
    }

    ranges
}

/// Parse only real probe replies, never OSC-looking content inside a bracketed paste.
pub(super) fn terminal_default_colors(input: &[u8]) -> Option<super::DefaultColors> {
    let mut foreground = None;
    let mut background = None;

    for range in terminal_response_ranges(input) {
        let response = &input[range];
        if foreground.is_none() {
            foreground = super::parse_osc_color(response, /*slot*/ 10);
        }
        if background.is_none() {
            background = super::parse_osc_color(response, /*slot*/ 11);
        }
    }

    Some(super::DefaultColors {
        fg: foreground?,
        bg: background?,
    })
}

/// Decode an XTWINOPS character-cell reply, in pixels, outside bracketed paste.
pub(super) fn terminal_cell_size(input: &[u8]) -> Option<(u16, u16)> {
    terminal_response_ranges(input)
        .into_iter()
        .find_map(|range| parse_cell_size_response(&input[range]))
}

fn parse_cell_size_response(response: &[u8]) -> Option<(u16, u16)> {
    let payload = response.strip_prefix(b"\x1b[6;")?.strip_suffix(b"t")?;
    let payload = std::str::from_utf8(payload).ok()?;
    let (height, width) = payload.split_once(';')?;
    let dimension = |value: &str| {
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        value.parse::<u16>().ok().filter(|value| *value != 0)
    };
    Some((dimension(width)?, dimension(height)?))
}

#[cfg(test)]
#[path = "windows_replay_tests.rs"]
mod tests;
