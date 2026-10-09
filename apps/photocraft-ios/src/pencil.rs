//! Apple Pencil double-tap and squeeze, with the action the user picked in Settings › Apple
//! Pencil, as Photoshop on the iPad does them:
//!
//! - **Switch between current tool and eraser**: the Eraser, and back to the tool before it.
//! - **Switch between current tool and last used**: the previous tool, and back.
//! - **Show colour palette**: the Color Picker for the foreground colour.
//! - **Show ink attributes** / **contextual palette**: the Brush Settings panel.
//! - **Off** / **Run system shortcut**: nothing (the system runs its shortcut itself).

use photocraft_tablet::uikit::PencilAction;
use photocraft_ui_egui::{PhotocraftApp, Tool};

/// Tracks the tools the Pencil's barrel switches between.
#[derive(Debug, Default)]
pub struct PencilTools {
    /// The tool seen last frame, and the one before it.
    current: Option<Tool>,
    previous: Option<Tool>,
    /// The tool to return to when the barrel switches away from the Eraser it chose.
    before_eraser: Option<Tool>,
}

impl PencilTools {
    /// Call every frame with the current tool: remembers the tool used before it.
    pub fn track(&mut self, tool: Tool) {
        if self.current != Some(tool) {
            self.previous = self.current.or(self.previous);
            self.current = Some(tool);
        }
    }

    /// Carry out a barrel action. Returns what it did, for the status bar (`None`: nothing).
    pub fn apply(&mut self, app: &mut PhotocraftApp, ctx: &egui::Context, action: PencilAction) -> Option<&'static str> {
        self.track(app.ui.tool);
        match action {
            PencilAction::SwitchEraser => {
                if app.ui.tool == Tool::Eraser {
                    app.ui.tool = self.before_eraser.take().or(self.previous).filter(|t| *t != Tool::Eraser).unwrap_or(Tool::Brush);
                } else {
                    self.before_eraser = Some(app.ui.tool);
                    app.ui.tool = Tool::Eraser;
                }
                self.track(app.ui.tool);
                Some("Apple Pencil: switched tools")
            }
            PencilAction::SwitchPrevious => {
                let previous = self.previous.filter(|t| *t != app.ui.tool)?;
                app.ui.tool = previous;
                self.track(app.ui.tool);
                Some("Apple Pencil: switched to the previous tool")
            }
            PencilAction::ShowColorPalette => {
                photocraft_ui_egui::color_picker_ui::open(app, "foreground");
                Some("Apple Pencil: Color Picker")
            }
            PencilAction::ShowInkAttributes | PencilAction::ShowContextualPalette => {
                photocraft_ui_egui::shortcut_dispatch::dispatch(app, ctx, "window.toggle.brushSettings");
                Some("Apple Pencil: Brush Settings")
            }
            PencilAction::Ignore | PencilAction::RunSystemShortcut => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_engine::Session;

    fn app() -> PhotocraftApp {
        PhotocraftApp::new(Session::new(), Default::default())
    }

    #[test]
    fn double_tap_toggles_the_eraser() {
        let (mut app, ctx, mut t) = (app(), egui::Context::default(), PencilTools::default());
        app.ui.tool = Tool::Brush;
        t.track(app.ui.tool);
        assert!(t.apply(&mut app, &ctx, PencilAction::SwitchEraser).is_some());
        assert_eq!(app.ui.tool, Tool::Eraser);
        t.apply(&mut app, &ctx, PencilAction::SwitchEraser);
        assert_eq!(app.ui.tool, Tool::Brush, "back to the tool before the eraser");
        // Picked the Eraser by hand: the barrel goes back to the tool used before it.
        app.ui.tool = Tool::Hand;
        t.track(app.ui.tool);
        app.ui.tool = Tool::Eraser;
        t.track(app.ui.tool);
        t.apply(&mut app, &ctx, PencilAction::SwitchEraser);
        assert_eq!(app.ui.tool, Tool::Hand);
        // No tool before it at all: the Brush.
        let mut fresh = PencilTools::default();
        app.ui.tool = Tool::Eraser;
        fresh.apply(&mut app, &ctx, PencilAction::SwitchEraser);
        assert_eq!(app.ui.tool, Tool::Brush);
    }

    #[test]
    fn double_tap_switches_to_the_previous_tool_and_back() {
        let (mut app, ctx, mut t) = (app(), egui::Context::default(), PencilTools::default());
        app.ui.tool = Tool::Brush;
        t.track(app.ui.tool);
        app.ui.tool = Tool::Hand;
        t.track(app.ui.tool);
        t.apply(&mut app, &ctx, PencilAction::SwitchPrevious);
        assert_eq!(app.ui.tool, Tool::Brush);
        t.apply(&mut app, &ctx, PencilAction::SwitchPrevious);
        assert_eq!(app.ui.tool, Tool::Hand);
        // Nothing to go back to: no change.
        let mut fresh = PencilTools::default();
        assert_eq!(fresh.apply(&mut app, &ctx, PencilAction::SwitchPrevious), None);
        assert_eq!(app.ui.tool, Tool::Hand);
    }

    #[test]
    fn off_and_system_shortcuts_do_nothing() {
        let (mut app, ctx, mut t) = (app(), egui::Context::default(), PencilTools::default());
        app.ui.tool = Tool::Brush;
        assert_eq!(t.apply(&mut app, &ctx, PencilAction::Ignore), None);
        assert_eq!(t.apply(&mut app, &ctx, PencilAction::RunSystemShortcut), None);
        assert_eq!(app.ui.tool, Tool::Brush);
    }

    #[test]
    fn palettes_open_without_panicking() {
        let (mut app, ctx, mut t) = (app(), egui::Context::default(), PencilTools::default());
        assert!(t.apply(&mut app, &ctx, PencilAction::ShowColorPalette).is_some());
        assert!(t.apply(&mut app, &ctx, PencilAction::ShowInkAttributes).is_some());
        assert!(t.apply(&mut app, &ctx, PencilAction::ShowContextualPalette).is_some());
    }
}
