//! Interpret `/image`'s single path argument without shell expansion or execution.

use std::path::Path;
use std::path::PathBuf;

pub(crate) fn resolve_image_path(argument: &str, cwd: &Path) -> PathBuf {
    let argument = argument.trim();
    let unquoted = argument
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .or_else(|| {
            argument
                .strip_prefix('\'')
                .and_then(|value| value.strip_suffix('\''))
        })
        .unwrap_or(argument);
    let path = Path::new(unquoted);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_with_spaces_and_shell_metacharacters_are_literal() {
        let cwd = Path::new("workspace");
        for argument in [
            "images/my screenshot.png",
            "\"images/my screenshot.png\"",
            "'images/my screenshot.png'",
        ] {
            assert_eq!(
                resolve_image_path(argument, cwd),
                cwd.join("images/my screenshot.png")
            );
        }
        assert_eq!(
            resolve_image_path("$(command); image.png", cwd),
            cwd.join("$(command); image.png")
        );
    }

    #[test]
    fn absolute_paths_are_not_rebased() {
        let root = std::env::temp_dir();
        let image = root.join("image preview.png");
        assert_eq!(
            resolve_image_path(&format!("\"{}\"", image.display()), Path::new("elsewhere")),
            image
        );
    }

    #[test]
    fn windows_backslashes_are_preserved() {
        let cwd = Path::new("workspace");
        assert_eq!(
            resolve_image_path(r"images\new\test.png", cwd),
            cwd.join(r"images\new\test.png")
        );
    }
}
