//! The iPad's [`Services`]: the app's folders, file import and export, crash-safe writes,
//! preferences and crash-recovery autosave.
//!
//! An iPad app lives in its sandbox. `$HOME` is the app's container: settings, presets and
//! autosaves go to `Library/Application Support/PhotoCraft`, documents to `Documents`, which the
//! Files app shows as *On My iPad › PhotoCraft* (Info.plist: `UIFileSharingEnabled`,
//! `LSSupportsOpeningDocumentsInPlace`). File › Open and Save As browse that folder in the app
//! (see [`crate::files`]).
//!
//! Off the iPad (running this shell on a desktop to try it), both live in a temporary folder
//! unless `PHOTOCRAFT_CONFIG_DIR` / `PHOTOCRAFT_DOCUMENTS_DIR` say otherwise, so a trial never
//! touches the desktop app's settings.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use photocraft_codecs::{ChannelLayout, EncodeOptions, Image};
use photocraft_doc::Document;
use photocraft_format::RecoveryStore;
use photocraft_ui_egui::{FileDialogFn, Recovered, Services};

/// The app's folders.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dirs {
    /// Settings, presets, autosaves and logs.
    pub config: PathBuf,
    /// The user's documents (the Files app's *On My iPad › PhotoCraft*).
    pub documents: PathBuf,
}

impl Dirs {
    /// The folders for this process (see the module docs).
    pub fn current() -> Option<Self> {
        Self::resolve(|k| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from), cfg!(target_os = "ios"), std::env::temp_dir())
    }

    /// [`Self::current`] from an environment lookup, whether this is the iPad, and the temporary
    /// folder.
    pub fn resolve(env: impl Fn(&str) -> Option<PathBuf>, ios: bool, temp: PathBuf) -> Option<Self> {
        let (config, documents) = if ios {
            let home = env("HOME").filter(|h| h.is_absolute())?;
            (home.join("Library").join("Application Support").join("PhotoCraft"), home.join("Documents"))
        } else {
            let base = temp.join("photocraft-ios");
            (base.join("config"), base.join("Documents"))
        };
        Some(Self {
            config: env("PHOTOCRAFT_CONFIG_DIR").filter(|p| p.is_absolute()).unwrap_or(config),
            documents: env("PHOTOCRAFT_DOCUMENTS_DIR").filter(|p| p.is_absolute()).unwrap_or(documents),
        })
    }

    pub fn prefs_file(&self) -> PathBuf {
        self.config.join("preferences.json")
    }

    pub fn presets(&self) -> PathBuf {
        self.config.join("Presets")
    }

    fn recovery(&self) -> PathBuf {
        self.config.join("Recovery")
    }

    /// Create both folders (an iPad app's container starts with `Documents` only).
    pub fn create(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.config)?;
        std::fs::create_dir_all(&self.documents)
    }
}

/// Write `bytes` crash-safely (temp file, fsync, rename; see [`photocraft_format::atomic`]): a
/// failed or interrupted save never destroys the old file. iPadOS suspends and may end a
/// backgrounded app at any moment, so this matters even more than on the desktop.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    photocraft_format::atomic_write(path, bytes).map_err(|e| e.to_string())
}

/// A panic during file import or export becomes an error the UI shows; the open documents stay.
fn guard<T>(what: &str, f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
        .unwrap_or_else(|_| Err(format!("{what} failed with an internal error (logged); your open documents are unchanged")))
}

type SharedRecovery = Rc<RefCell<Option<RecoveryStore>>>;

fn with_store<R>(store: &SharedRecovery, f: impl FnOnce(&mut RecoveryStore) -> R) -> Result<R, String> {
    let mut slot = store.try_borrow_mut().map_err(|_| "crash recovery is busy".to_string())?;
    Ok(f(slot.as_mut().ok_or("no config directory")?))
}

/// The services, with File › Open and Save As answered by `file_dialog`.
pub fn ios(dirs: Option<&Dirs>, file_dialog: FileDialogFn) -> Services {
    let store: SharedRecovery = Rc::new(RefCell::new(dirs.map(|d| RecoveryStore::new(d.recovery()))));
    let (s1, s2, s3, s4) = (store.clone(), store.clone(), store.clone(), store.clone());
    let prefs = dirs.map(Dirs::prefs_file);
    let save_to = prefs.clone();
    Services {
        import: Some(Box::new(|name: &str, bytes: &[u8]| {
            guard("Open", || photocraft_io::import(name, bytes).map(|r| (r.document, r.warnings)).map_err(|e| e.to_string()))
        })),
        export: Some(Box::new(|doc: &Document, path: &str, settings: &photocraft_ui_egui::ExportSettings| {
            let mut opts = photocraft_io::ExportOptions::default();
            if let Some(q) = settings.jpeg_quality {
                opts.encode.jpeg_quality = q;
            }
            opts.encode.webp_lossless = settings.webp_lossless;
            if let Some(q) = settings.webp_quality {
                opts.encode.webp_quality = q;
            }
            opts.tiff_layers = settings.tiff_layers;
            opts.xmp = if settings.xmp_all { photocraft_io::XmpEmbed::All } else { photocraft_io::XmpEmbed::None };
            guard("Export", || photocraft_io::export(doc, path, &opts).map(|r| (r.bytes, r.warnings)).map_err(|e| e.to_string()))
        })),
        file_dialog: Some(file_dialog),
        write: Some(Box::new(|path: &str, bytes: &[u8]| write_atomic(Path::new(path), bytes))),
        encode_png: Some(Box::new(|w, h, rgba| {
            let img = Image::from_u8(w, h, ChannelLayout::Rgba, rgba.to_vec()).map_err(|e| e.to_string())?;
            photocraft_codecs::encode(&img, photocraft_codecs::Format::Png, &EncodeOptions::default()).map_err(|e| e.to_string())
        })),
        load_prefs: Some(Box::new(move || std::fs::read_to_string(prefs.as_ref()?).ok())),
        save_prefs: Some(Box::new(move |text: &str| write_atomic(save_to.as_deref().ok_or("no config directory")?, text.as_bytes()))),
        append_text: Some(Box::new(|path: &str, text: &str| {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path).map_err(|e| e.to_string())?;
            f.write_all(text.as_bytes()).map_err(|e| e.to_string())
        })),
        autosave: Some(Box::new(move |doc: &Arc<Document>, revision: u64, path: Option<&str>| {
            with_store(&s1, |s| s.autosave_checked(doc, revision, path.map(str::to_string)))?
        })),
        autosave_results: Some(Box::new(move || with_store(&s4, |s| s.take_completed()).unwrap_or_default())),
        discard_autosave: Some(Box::new(move |id: u64| {
            let _ = with_store(&s2, |s| s.discard(id));
        })),
        recover: Some(Box::new(move || {
            let found = with_store(&s3, |s| s.recover()).unwrap_or_default();
            found.into_iter().map(|(e, doc)| Recovered { key: e.info.key, path: e.info.original_path, doc }).collect()
        })),
        adopt_autosave: Some(Box::new(move |id: u64, key: &str| {
            let _ = with_store(&store, |s| s.adopt(id, key));
        })),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<PathBuf> + 'a {
        move |k| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| PathBuf::from(v))
    }

    // Unix paths: `/var/…` isn't absolute on Windows (no drive), and the iPad is Unix.
    #[cfg(unix)]
    #[test]
    fn ipad_folders_live_in_the_app_container() {
        let d = Dirs::resolve(env(&[("HOME", "/var/mobile/Containers/Data/Application/X")]), true, "/tmp".into()).unwrap();
        assert_eq!(d.config, Path::new("/var/mobile/Containers/Data/Application/X/Library/Application Support/PhotoCraft"));
        assert_eq!(d.documents, Path::new("/var/mobile/Containers/Data/Application/X/Documents"));
        assert_eq!(d.prefs_file(), d.config.join("preferences.json"));
        // No usable $HOME: no folders (everything still runs, nothing persists).
        assert_eq!(Dirs::resolve(env(&[]), true, "/tmp".into()), None);
        assert_eq!(Dirs::resolve(env(&[("HOME", "relative")]), true, "/tmp".into()), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_desktop_trial_never_touches_real_settings() {
        let d = Dirs::resolve(env(&[("HOME", "/home/me")]), false, "/tmp".into()).unwrap();
        assert_eq!(d.config, Path::new("/tmp/photocraft-ios/config"));
        assert_eq!(d.documents, Path::new("/tmp/photocraft-ios/Documents"));
        let d = Dirs::resolve(env(&[("PHOTOCRAFT_CONFIG_DIR", "/c"), ("PHOTOCRAFT_DOCUMENTS_DIR", "/d")]), false, "/tmp".into()).unwrap();
        assert_eq!((d.config.as_path(), d.documents.as_path()), (Path::new("/c"), Path::new("/d")));
        // Relative overrides are ignored.
        let d = Dirs::resolve(env(&[("PHOTOCRAFT_CONFIG_DIR", "c")]), false, "/tmp".into()).unwrap();
        assert_eq!(d.config, Path::new("/tmp/photocraft-ios/config"));
    }

    #[test]
    fn writes_create_the_folder_and_replace_atomically() {
        let dir = std::env::temp_dir().join(format!("pc-ios-write-{}", std::process::id()));
        let file = dir.join("sub").join("a.txt");
        write_atomic(&file, b"one").unwrap();
        write_atomic(&file, b"two").unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"two");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn guard_turns_a_panic_into_an_error() {
        let r: Result<(), String> = guard("Open", || panic!("boom"));
        assert!(r.unwrap_err().contains("Open failed"));
    }
}
