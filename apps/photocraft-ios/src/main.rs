//! PhotoCraft for iPad.
//!
//! The same [`photocraft_ui_egui::PhotocraftApp`] as the desktop app, in eframe's native runner
//! (winit + wgpu on Metal), with an iPad shell around it:
//!
//! - **Apple Pencil**: pressure, tilt (altitude + azimuth), barrel roll (Apple Pencil Pro), hover
//!   and double-tap / squeeze ([`photocraft_tablet::ios`], [`pencil`]);
//! - **touch**: the Pencil draws, fingers pan and pinch-zoom, two- and three-finger taps undo and
//!   redo ([`touch`]);
//! - **files**: File › Open and Save As browse the app's Documents folder, which the Files app
//!   shows as *On My iPad › PhotoCraft* ([`files`], [`services`]).
//!
//! Build and run it with `packaging/ios/build.sh` (see `docs/ios.md`). On a desktop,
//! `cargo run -p photocraft-ios` runs the same shell in an iPad-sized window to try the shell
//! itself (touch input needs a touch screen there, and there is no Pencil).

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod files;
mod pencil;
mod services;
mod shell;
mod touch;

/// Log records to standard error, which Xcode's console and `xcrun devicectl device process
/// launch --console` show: PhotoCraft's own at Info and above, everyone else's at Warn.
struct Logger;

impl log::Log for Logger {
    fn enabled(&self, m: &log::Metadata<'_>) -> bool {
        let ours = m.target().starts_with("photocraft");
        m.level() <= if ours { log::Level::Info } else { log::Level::Warn }
    }

    fn log(&self, r: &log::Record<'_>) {
        if self.enabled(r.metadata()) {
            use std::io::Write as _;
            let _ = writeln!(std::io::stderr(), "photocraft: [{}] {}: {}", r.level(), r.target(), r.args());
        }
    }

    fn flush(&self) {}
}

static LOGGER: Logger = Logger;

fn main() -> eframe::Result {
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
    // A panic that escapes anyway is logged before the default hook reports it.
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::error!("PhotoCraft internal error: {info}");
        default(info);
    }));
    log::info!("PhotoCraft {} for iPad", photocraft_engine::build_info::long_version());
    shell::run()
}
