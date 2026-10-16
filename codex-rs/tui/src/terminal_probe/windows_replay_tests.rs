use super::MAX_TERMINAL_COLOR_RESPONSE_BYTES;
use super::terminal_cell_size;
use super::terminal_default_colors;
use super::terminal_response_ranges;
use crate::terminal_probe::DefaultColors;
use pretty_assertions::assert_eq;

fn preserved_input(input: &[u8]) -> Vec<u8> {
    let ranges = terminal_response_ranges(input);
    input
        .iter()
        .enumerate()
        .filter_map(|(index, byte)| {
            (!ranges.iter().any(|range| range.contains(&index))).then_some(*byte)
        })
        .collect()
}

#[test]
fn preserves_typeahead_before_between_and_after_terminal_color_replies() {
    let input = b"before\x1b]10;rgb:5555/5757/5353\x1b\\between\x1b]11;rgb:ffff/ffff/ffff\x07after";

    assert_eq!(preserved_input(input), b"beforebetweenafter");
}

#[test]
fn preserves_unrelated_malformed_and_partial_osc_sequences() {
    let input = b"\x1b]52;clipboard\x07\x1b]10;not-a-color\x07typed\x1b]11;rgb:ffff/ffff";

    assert_eq!(preserved_input(input), input);
}

#[test]
fn preserves_terminal_color_response_that_exceeds_the_byte_limit() {
    let mut input = b"prefix\x1b]10;".to_vec();
    input.extend(std::iter::repeat_n(b'x', MAX_TERMINAL_COLOR_RESPONSE_BYTES));
    input.extend_from_slice(b"\x07suffix");

    assert_eq!(preserved_input(&input), input);
}

#[test]
fn finds_terminal_color_replies_after_unrelated_escape_sequences() {
    let input = b"\x1b]52;clipboard\x07typed\x1b]11;rgba:ffff/eeee/dddd/ffff\x1b\\suffix";

    assert_eq!(preserved_input(input), b"\x1b]52;clipboard\x07typedsuffix");
}

#[test]
fn consumes_one_and_three_digit_terminal_color_replies() {
    let input = b"before\x1b]10;rgb:f/e/d\x07middle\x1b]11;rgb:fff/800/000\x1b\\after";

    assert_eq!(preserved_input(input), b"beforemiddleafter");
    assert_eq!(
        terminal_default_colors(input),
        Some(DefaultColors {
            fg: (255, 238, 221),
            bg: (255, 127, 0),
        })
    );
}

#[test]
fn preserves_complete_osc_looking_bracketed_paste() {
    let input = b"before\x1b[200~\x1b]10;rgb:f/e/d\x07\x1b]11;rgb:fff/800/000\x07\x1b[201~after";

    assert_eq!(preserved_input(input), input);
    assert_eq!(terminal_default_colors(input), None);
}

#[test]
fn preserves_unfinished_osc_looking_bracketed_paste() {
    let input = b"before\x1b[200~\x1b]10;rgb:f/e/d\x07\x1b]11;rgb:fff/800/000\x07";

    assert_eq!(preserved_input(input), input);
    assert_eq!(terminal_default_colors(input), None);
}

#[test]
fn parses_real_color_replies_around_bracketed_paste() {
    let input =
        b"\x1b]10;rgb:f/e/d\x07\x1b[200~\x1b]11;rgb:0/0/0\x07\x1b[201~\x1b]11;rgb:fff/800/000\x07";

    assert_eq!(
        preserved_input(input),
        b"\x1b[200~\x1b]11;rgb:0/0/0\x07\x1b[201~"
    );
    assert_eq!(
        terminal_default_colors(input),
        Some(DefaultColors {
            fg: (255, 238, 221),
            bg: (255, 127, 0),
        })
    );
}

#[test]
fn parses_cell_dimensions_and_preserves_interleaved_typeahead() {
    let input = b"before\x1b]10;rgb:f/e/d\x07between\x1b[6;20;10tafter";

    assert_eq!(terminal_cell_size(input), Some((10, 20)));
    assert_eq!(preserved_input(input), b"beforebetweenafter");
}

#[test]
fn preserves_invalid_and_unfinished_cell_size_responses() {
    for input in [
        b"\x1b[6;0;10t".as_slice(),
        b"\x1b[6;20;0t",
        b"\x1b[6;-20;10t",
        b"\x1b[6;+20;10t",
        b"\x1b[6;65536;10t",
        b"\x1b[6;20;65536t",
        b"\x1b[6;20;10;1t",
        b"\x1b[6;20;t",
        b"\x1b[6;;10t",
        b"\x1b[6;20;10",
        b"\x1b[4;20;10t",
    ] {
        assert_eq!(terminal_cell_size(input), None);
        assert_eq!(preserved_input(input), input);
    }
}

#[test]
fn parses_cell_size_after_incomplete_csi_and_unrelated_keys() {
    let input = b"\x1b[6;unfinished\x1b[Atyped\x1b[6;18;9t";

    assert_eq!(terminal_cell_size(input), Some((9, 18)));
    assert_eq!(preserved_input(input), b"\x1b[6;unfinished\x1b[Atyped");
}

#[test]
fn preserves_cell_size_looking_bracketed_pastes() {
    for input in [
        b"before\x1b[200~\x1b[6;20;10t\x1b[201~after".as_slice(),
        b"before\x1b[200~\x1b[6;20;10t",
    ] {
        assert_eq!(terminal_cell_size(input), None);
        assert_eq!(preserved_input(input), input);
    }
}

#[test]
fn finds_real_cell_size_reply_after_bracketed_paste() {
    let input = b"\x1b[200~\x1b[6;90;90t\x1b[201~\x1b[6;20;10t";

    assert_eq!(terminal_cell_size(input), Some((10, 20)));
    assert_eq!(preserved_input(input), b"\x1b[200~\x1b[6;90;90t\x1b[201~");
}
