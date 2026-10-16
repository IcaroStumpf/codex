//! Open a local image gallery using the conversation's existing artifact paths.

use super::*;
use crate::history_cell::UserHistoryCell;
use crate::image_preview::ImagePreviewOverlay;

impl App {
    pub(super) fn open_image_preview(
        &mut self,
        tui: &mut tui::Tui,
        path: Option<PathBuf>,
    ) -> Result<()> {
        // A remote workspace path may name an unrelated file on the TUI host.
        // Only explicitly supplied local paths can be opened in remote sessions.
        let local_history = !self.app_server_target.uses_remote_workspace()
            && crate::projectless::has_only_local_environments(&self.environment_manager);
        let paths = match path {
            Some(path) => vec![path],
            None if local_history => conversation_image_paths(&self.transcript_cells),
            None => {
                self.chat_widget.add_error_message(
                    "Conversation image previews require a local workspace. Use /image <local-path> to preview a file on this computer.".to_string(),
                );
                return Ok(());
            }
        };
        if paths.is_empty() {
            self.chat_widget.add_error_message(
                "No local images in this conversation yet. Use /image <path> to preview an image file.".to_string(),
            );
            return Ok(());
        }
        let protocol = self.chat_widget.pet_image_support().protocol();
        #[cfg(windows)]
        if protocol == Some(crate::pets::ImageProtocol::Sixel) {
            tui.probe_default_colors_after_protected_startup();
        }
        tui.enter_alt_screen()?;
        self.overlay = Some(Overlay::Image(Box::new(ImagePreviewOverlay::new(
            paths,
            protocol,
            self.keymap.pager.clone(),
        ))));
        tui.frame_requester().schedule_frame();
        Ok(())
    }
}

/// Keep the latest occurrence of an image first, including user attachments.
fn conversation_image_paths(cells: &[Arc<dyn HistoryCell>]) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut paths = Vec::new();
    for cell in cells.iter().rev() {
        let cell_paths = if let Some(user) = cell.as_any().downcast_ref::<UserHistoryCell>() {
            user.local_image_paths.clone()
        } else {
            cell.image_path()
                .map(|path| vec![path.to_path_buf()])
                .unwrap_or_default()
        };
        for path in cell_paths {
            if seen.insert(path.clone()) {
                paths.push(path);
            }
        }
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gallery_includes_generated_viewed_and_attached_images_latest_first() {
        let root = std::env::temp_dir();
        let attached = root.join("attached.png");
        let generated = root.join("generated.png");
        let generated_absolute = AbsolutePathBuf::try_from(generated.clone()).unwrap();
        let cells: Vec<Arc<dyn HistoryCell>> = vec![
            Arc::new(history_cell::new_user_prompt(
                "look".to_string(),
                Vec::new(),
                vec![attached.clone()],
                Vec::new(),
            )),
            Arc::new(history_cell::new_image_generation_call(
                "generated".to_string(),
                "completed",
                None,
                Some(generated_absolute),
            )),
            Arc::new(history_cell::new_view_image_tool_call(
                "viewed".to_string(),
                codex_utils_path_uri::LegacyAppPathString::from_string(
                    attached.display().to_string(),
                ),
            )),
        ];
        assert_eq!(conversation_image_paths(&cells), vec![attached, generated]);
    }

    #[test]
    fn failed_generation_and_remote_only_attachments_are_not_local_images() {
        let root = std::env::temp_dir();
        let cells: Vec<Arc<dyn HistoryCell>> = vec![
            Arc::new(history_cell::new_user_prompt(
                String::new(),
                Vec::new(),
                Vec::new(),
                vec!["https://example.com/image.png".to_string()],
            )),
            Arc::new(history_cell::new_image_generation_call(
                "failed".to_string(),
                "failed",
                None,
                Some(AbsolutePathBuf::try_from(root.join("failed.png")).unwrap()),
            )),
        ];
        assert!(conversation_image_paths(&cells).is_empty());
    }
}
