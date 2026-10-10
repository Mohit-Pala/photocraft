//! File › Open and Save As on the iPad: a browser of the app's Documents folder, drawn by the
//! app. (The system's document picker needs UIKit view controllers and a delegate, a later step;
//! this needs no platform code.) Files get into the folder through the Files app (*On My iPad ›
//! PhotoCraft*), AirDrop to Files, or Finder's file sharing over USB, and saved files show up
//! there.
//!
//! The app asks through [`Services::file_dialog`](photocraft_ui_egui::Services::file_dialog);
//! [`Browser::show`] draws the open request every frame and answers through its reply.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use photocraft_ui_egui::{FileDialogAnswer, FileDialogFn, FileDialogReply, FileDialogRequest};

/// One open dialog.
struct Pending {
    reply: FileDialogReply,
    save: bool,
    multiple: bool,
    /// The folder shown, inside the root.
    dir: PathBuf,
    /// Save As: the file name typed.
    name: String,
    /// Open: the chosen files.
    chosen: Vec<PathBuf>,
}

/// The browser's state, shared with the file-dialog service.
pub struct Browser {
    root: Option<PathBuf>,
    pending: Option<Pending>,
}

pub type Shared = Rc<RefCell<Browser>>;

impl Browser {
    /// A browser of `root` (`None`: no Documents folder, every request is cancelled).
    pub fn new(root: Option<PathBuf>) -> Shared {
        Rc::new(RefCell::new(Self { root, pending: None }))
    }

    /// The [`FileDialogFn`] that queues requests for [`Self::show`].
    pub fn service(shared: &Shared) -> FileDialogFn {
        let shared = shared.clone();
        Box::new(move |request: FileDialogRequest, _frame: Option<&eframe::Frame>, reply: FileDialogReply| match shared.try_borrow_mut() {
            Ok(mut b) => b.begin(request, reply),
            // Busy (can't happen: the browser only borrows inside `show`): cancel.
            Err(_) => reply.send(None),
        })
    }

    fn begin(&mut self, request: FileDialogRequest, reply: FileDialogReply) {
        let Some(root) = self.root.clone() else {
            reply.send(None);
            return;
        };
        // A request while one is open cancels the old one (the app allows one at a time anyway).
        if let Some(old) = self.pending.take() {
            old.reply.send(None);
        }
        self.pending = Some(match request {
            FileDialogRequest::Open { multiple, initial_dir } => {
                let dir = initial_dir.map(PathBuf::from).filter(|d| inside(&root, d) && d.is_dir()).unwrap_or_else(|| root.clone());
                Pending { reply, save: false, multiple, dir, name: String::new(), chosen: Vec::new() }
            }
            FileDialogRequest::Save { suggested } => {
                let suggested = Path::new(&suggested);
                let name = suggested.file_name().map_or_else(|| "Untitled.pcraft".to_string(), |n| n.to_string_lossy().into_owned());
                let dir = suggested.parent().map(Path::to_path_buf).filter(|d| inside(&root, d) && d.is_dir()).unwrap_or_else(|| root.clone());
                Pending { reply, save: true, multiple: false, dir, name, chosen: Vec::new() }
            }
        });
    }

    /// Draw the open dialog, if any, and answer it when the user is done.
    pub fn show(shared: &Shared, ctx: &egui::Context) {
        let Ok(mut b) = shared.try_borrow_mut() else { return };
        let Some(root) = b.root.clone() else { return };
        let Some(mut p) = b.pending.take() else { return };
        let mut answer: Option<Option<FileDialogAnswer>> = None;
        let title = if p.save { "Save to On My iPad › PhotoCraft" } else { "Open from On My iPad › PhotoCraft" };
        egui::Modal::new(egui::Id::new("pc-ios-files")).show(ctx, |ui| {
            ui.set_width(ui.available_width().clamp(320.0, 560.0));
            ui.heading(title);
            ui.horizontal(|ui| {
                if p.dir != root && ui.button("⬆ Up").clicked() {
                    p.dir = p.dir.parent().map(Path::to_path_buf).filter(|d| inside(&root, d)).unwrap_or_else(|| root.clone());
                    p.chosen.clear();
                }
                let shown = p.dir.strip_prefix(&root).map(|r| r.display().to_string()).unwrap_or_default();
                ui.label(if shown.is_empty() { "PhotoCraft".to_string() } else { format!("PhotoCraft/{shown}") });
            });
            ui.separator();
            let entries = list(&p.dir);
            egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                if entries.is_empty() {
                    ui.weak(if p.save {
                        "This folder is empty."
                    } else {
                        "No files yet. Copy images and documents into On My iPad › PhotoCraft with the Files app, then open them here."
                    });
                }
                for e in &entries {
                    let label = if e.dir { format!("📁 {}", e.name) } else { e.name.clone() };
                    let selected = p.chosen.contains(&e.path) || (p.save && !e.dir && e.name == p.name);
                    let r = ui.add_sized([ui.available_width(), 36.0], egui::Button::selectable(selected, label));
                    if r.clicked() {
                        if e.dir {
                            p.dir = e.path.clone();
                            p.chosen.clear();
                        } else if p.save {
                            p.name = e.name.clone();
                        } else if p.multiple && p.chosen.contains(&e.path) {
                            p.chosen.retain(|c| *c != e.path);
                        } else if p.multiple {
                            p.chosen.push(e.path.clone());
                        } else {
                            p.chosen = vec![e.path.clone()];
                        }
                    }
                    if r.double_clicked() && !e.dir && !p.save {
                        answer = Some(Some(FileDialogAnswer::Paths(vec![path_string(&e.path)])));
                    }
                }
            });
            ui.separator();
            if p.save {
                ui.horizontal(|ui| {
                    ui.label("Name:");
                    ui.add(egui::TextEdit::singleline(&mut p.name).desired_width(f32::INFINITY));
                });
            }
            let target = save_target(&p.dir, &p.name);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    answer = Some(None);
                }
                if p.save {
                    let replace = target.as_ref().is_some_and(|t| t.exists());
                    let label = if replace { "Replace" } else { "Save" };
                    if ui.add_enabled(target.is_some(), egui::Button::new(label)).clicked()
                        && let Some(t) = &target
                    {
                        answer = Some(Some(FileDialogAnswer::SaveTo(path_string(t))));
                    }
                } else if ui.add_enabled(!p.chosen.is_empty(), egui::Button::new("Open")).clicked() {
                    answer = Some(Some(FileDialogAnswer::Paths(p.chosen.iter().map(|c| path_string(c.as_path())).collect())));
                }
            });
        });
        match answer {
            Some(a) => p.reply.send(a),
            None => b.pending = Some(p),
        }
    }
}

/// A folder entry.
#[derive(Debug, PartialEq, Eq)]
struct Entry {
    name: String,
    path: PathBuf,
    dir: bool,
}

/// The folder's visible entries: folders first, then files, by name; hidden files left out.
fn list(dir: &Path) -> Vec<Entry> {
    let Ok(read) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut v: Vec<Entry> = read
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            (!name.starts_with('.')).then(|| Entry { dir: e.path().is_dir(), path: e.path(), name })
        })
        .collect();
    v.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    v
}

/// Where Save writes `name` in `dir`: `None` for an empty name or one that would leave the
/// folder (`a/b`, `..`).
fn save_target(dir: &Path, name: &str) -> Option<PathBuf> {
    let name = name.trim();
    let plain = !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', '\0']);
    plain.then(|| dir.join(name))
}

/// Is `path` inside `root` (or `root` itself)? Lexical: the paths come from the browser itself.
fn inside(root: &Path, path: &Path) -> bool {
    path.starts_with(root) && !path.components().any(|c| c == std::path::Component::ParentDir)
}

fn path_string(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_names_stay_in_the_folder() {
        let d = Path::new("/docs");
        assert_eq!(save_target(d, " cat.png "), Some(PathBuf::from("/docs/cat.png")));
        for bad in ["", "  ", ".", "..", "a/b.png", "../x.png", "a\\b", "nul\0l"] {
            assert_eq!(save_target(d, bad), None, "{bad:?}");
        }
    }

    #[test]
    fn inside_rejects_escapes() {
        let root = Path::new("/docs");
        assert!(inside(root, Path::new("/docs")));
        assert!(inside(root, Path::new("/docs/a/b")));
        assert!(!inside(root, Path::new("/other")));
        assert!(!inside(root, Path::new("/docs/../etc")));
    }

    #[test]
    fn listing_puts_folders_first_and_hides_dotfiles() {
        let dir = std::env::temp_dir().join(format!("pc-ios-list-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Zeta")).unwrap();
        std::fs::write(dir.join("b.png"), b"x").unwrap();
        std::fs::write(dir.join("A.psd"), b"x").unwrap();
        std::fs::write(dir.join(".DS_Store"), b"x").unwrap();
        let names: Vec<String> = list(&dir).into_iter().map(|e| e.name).collect();
        assert_eq!(names, vec!["Zeta", "A.psd", "b.png"]);
        assert!(list(&dir.join("missing")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
