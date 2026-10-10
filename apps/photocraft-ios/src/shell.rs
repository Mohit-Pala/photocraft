//! The iPad shell around [`PhotocraftApp`]: touch input ([`crate::touch`]), the Apple Pencil
//! ([`photocraft_tablet::ios`] and [`crate::pencil`]), the Documents browser ([`crate::files`])
//! and the iPad's services ([`crate::services`]).

use std::cell::RefCell;
use std::rc::Rc;

use photocraft_engine::Session;
use photocraft_tablet::uikit::PencilAction;
use photocraft_ui_egui::PhotocraftApp;

use crate::files::{self, Browser};
use crate::pencil::PencilTools;
use crate::services::{self, Dirs};
use crate::touch::{Frame, Shortcut, TouchInput};

/// What the Pencil glue reported since the last frame (it runs inside UIKit's event handling,
/// between frames, on the main thread).
#[derive(Debug, Default)]
pub struct PencilInbox {
    /// The latest hover update, in UIKit points (`Some(None)`: hovering ended).
    pub hover: Option<Option<egui::Pos2>>,
    /// Double-taps and squeezes, with the user's preferred actions.
    pub actions: Vec<PencilAction>,
}

pub struct IosShell {
    app: PhotocraftApp,
    touch: TouchInput,
    tools: PencilTools,
    shortcuts: Vec<Shortcut>,
    files: files::Shared,
    inbox: Rc<RefCell<PencilInbox>>,
    /// Keeps the Pencil glue installed for the app's lifetime.
    #[cfg(target_os = "ios")]
    _pencil: Option<photocraft_tablet::ios::Pencil>,
}

/// Settings › Apple Pencil › Only Draw with Apple Pencil.
#[cfg(target_os = "ios")]
fn pencil_only_drawing() -> bool {
    photocraft_tablet::ios::prefers_pencil_only_drawing()
}

#[cfg(not(target_os = "ios"))]
fn pencil_only_drawing() -> bool {
    false
}

impl eframe::App for IosShell {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        // UIKit points to egui points (egui's zoom factor follows the `interface.uiScale` preference).
        let zoom = ctx.zoom_factor();
        let hover = self.inbox.try_borrow_mut().ok().and_then(|mut i| i.hover.take());
        if let Some(hover) = hover {
            self.touch.set_hover(hover.filter(|_| zoom > 0.0).map(|p| (p.to_vec2() / zoom).to_pos2()));
        }
        let canvas = self.app.last_canvas_rect;
        // On the canvas and not under a menu, popup or floating window.
        let on_canvas = |p: egui::Pos2| canvas.contains(p) && ctx.layer_id_at(p).is_none_or(|l| l.order == egui::Order::Background);
        let now = raw_input.time.unwrap_or_else(|| ctx.input(|i| i.time));
        let frame = Frame { now, pencil_only: pencil_only_drawing(), on_canvas: &on_canvas };
        let shortcuts = self.touch.filter(&mut raw_input.events, &frame);
        self.shortcuts.extend(shortcuts);
        eframe::App::raw_input_hook(&mut self.app, ctx, raw_input);
    }

    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        for s in std::mem::take(&mut self.shortcuts) {
            photocraft_ui_egui::shortcut_dispatch::dispatch(&mut self.app, ctx, s.command());
        }
        self.tools.track(self.app.ui.tool);
        let actions = self.inbox.try_borrow_mut().map(|mut i| std::mem::take(&mut i.actions)).unwrap_or_default();
        // Not in the middle of a stroke: the barrel acts between strokes, as on the iPad.
        if !self.touch.pointer_down() {
            for action in actions {
                if let Some(what) = self.tools.apply(&mut self.app, ctx, action) {
                    self.app.ui.status = what.to_string();
                    self.app.ui.status_error = false;
                }
            }
        }
        eframe::App::logic(&mut self.app, ctx, frame);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        eframe::App::ui(&mut self.app, ui, frame);
        Browser::show(&self.files, ui.ctx());
    }
}

/// The window: full screen on the iPad (UIKit decides); an iPad Pro 11" in landscape when the
/// shell runs on a desktop to try it.
fn native_options(dirs: Option<&Dirs>) -> eframe::NativeOptions {
    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_title("PhotoCraft").with_inner_size([1194.0, 834.0]).with_min_inner_size([640.0, 480.0]),
        persistence_path: dirs.map(|d| d.config.join("ui.ron")),
        ..Default::default()
    };
    // The adapter's real texture limits (egui asks for 8192 px), so big documents stay on the GPU.
    photocraft_ui_egui::gpu_canvas::use_adapter_limits(&mut options.wgpu_options.wgpu_setup);
    options
}

/// Start the app; on the iPad this never returns (UIKit owns the run loop).
pub fn run() -> eframe::Result {
    let dirs = Dirs::current();
    if let Some(d) = &dirs
        && let Err(e) = d.create()
    {
        log::warn!("couldn't create the app folders: {e}");
    }
    // Brush presets load in the background; the app attaches them when they arrive.
    let presets = dirs.as_ref().map(|d| photocraft_engine::preset_store::open_dir_async(d.presets()));
    let options = native_options(dirs.as_ref());
    eframe::run_native(
        "PhotoCraft",
        options,
        Box::new(move |cc| {
            let files = Browser::new(dirs.as_ref().map(|d| d.documents.clone()));
            let mut services = services::ios(dirs.as_ref(), Browser::service(&files));
            services.preset_store = presets;
            let mut app = PhotocraftApp::new(Session::new(), services);
            // Long commands and file opens run as background jobs with progress and Cancel.
            app.background_jobs = true;
            if let Some(rs) = cc.wgpu_render_state.clone() {
                app.perf.gpu_info.set_adapter(&rs.adapter.get_info());
                app.set_wgpu(rs);
            }
            let inbox = Rc::new(RefCell::new(PencilInbox::default()));
            #[cfg(target_os = "ios")]
            let pencil = ios::install_pencil(cc, &app, &inbox);
            Ok(Box::new(IosShell {
                app,
                touch: TouchInput::default(),
                tools: PencilTools::default(),
                shortcuts: Vec::new(),
                files,
                inbox,
                #[cfg(target_os = "ios")]
                _pencil: pencil,
            }))
        }),
    )
}

#[cfg(target_os = "ios")]
mod ios {
    use std::cell::RefCell;
    use std::rc::Rc;

    use photocraft_tablet::uikit::PencilEvent;
    use photocraft_ui_egui::PhotocraftApp;
    use photocraft_ui_egui::stylus::PenSample;

    use super::PencilInbox;

    /// Install the Apple Pencil glue on eframe's view: samples go to the app's stylus feed (read
    /// by the canvas like a desktop tablet's), hover and barrel actions to `inbox`. On failure
    /// this logs why: the Pencil then still draws with pressure (winit's touch force).
    pub fn install_pencil(cc: &eframe::CreationContext<'_>, app: &PhotocraftApp, inbox: &Rc<RefCell<PencilInbox>>) -> Option<photocraft_tablet::ios::Pencil> {
        let feed = app.stylus.feed.clone();
        let inbox = inbox.clone();
        let ctx = cc.egui_ctx.clone();
        let installed = photocraft_tablet::ios::Pencil::install(cc, move |event| {
            match event {
                PencilEvent::Sample(s) => {
                    feed.set(s.map(|s| PenSample { pressure: s.pressure, tilt_x: s.tilt_x, tilt_y: s.tilt_y, rotation: s.rotation, eraser: s.eraser }))
                }
                PencilEvent::Hover(p) => {
                    if let Ok(mut i) = inbox.try_borrow_mut() {
                        i.hover = Some(p.map(|[x, y]| egui::pos2(x, y)));
                    }
                }
                PencilEvent::Tap(action) | PencilEvent::Squeeze(action) => {
                    if let Ok(mut i) = inbox.try_borrow_mut() {
                        i.actions.push(action);
                    }
                }
            }
            ctx.request_repaint();
        });
        installed.map_err(|e| log::warn!("Apple Pencil: {e}")).ok()
    }
}
