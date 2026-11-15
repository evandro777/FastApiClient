//! Export and import of project files, driven by the Transfer dialog.
use std::{
    collections::HashSet,
    fs,
    io::{ErrorKind, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

use fastapiclient_core::{
    exchange::{self, ImportError},
    insomnia,
    model::Project,
};
use slint::{ComponentHandle, Model};

use crate::{Ctx, ExportRow, Main, flush, refresh, save};

/// A project file is a few kilobytes; this only stops a wrong path (a video, say) from being read into memory.
const MAX_IMPORT: u64 = 16 * 1024 * 1024;

fn expand(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(path),
    }
}

fn file_stem(name: &str) -> String {
    let stem: String = name
        .chars()
        .map(|ch| if ch.is_alphanumeric() || ch == '-' { ch } else { '_' })
        .collect();
    if stem.trim_matches('_').is_empty() {
        "project".into()
    } else {
        stem
    }
}

/// `mode`: 0 export, 1 import. Clears the last message; export lists the projects (the selected one ticked) and
/// suggests the home folder, import starts on the FastApiClient format.
pub fn prepare(ui: &Main, c: &Ctx, mode: i32) {
    ui.set_transfer_mode(mode);
    ui.set_transfer_kind(-1);
    ui.set_transfer_format(0);
    ui.set_transfer_count(0);
    let mut path = String::new();
    if mode == 0 {
        let app = c.app.borrow();
        let current = app.ws.sel.map(|s| s.project);
        let rows: Vec<ExportRow> = app
            .ws
            .projects
            .iter()
            .enumerate()
            .map(|(i, p)| ExportRow {
                name: p.name.clone().into(),
                checked: Some(i) == current,
            })
            .collect();
        ui.set_transfer_selected(rows.iter().filter(|r| r.checked).count() as i32);
        c.export_rows.set_vec(rows);
        path = "~/".into();
    }
    ui.set_transfer_path(path.into());
}

/// A project was ticked or unticked in the export list.
pub fn toggled(ui: &Main, c: &Ctx, index: i32, on: bool) {
    if let Ok(i) = usize::try_from(index)
        && let Some(mut row) = c.export_rows.row_data(i)
    {
        row.checked = on;
        c.export_rows.set_row_data(i, row);
    }
    ui.set_transfer_selected(c.export_rows.iter().filter(|r| r.checked).count() as i32);
}

/// Opens the desktop's chooser (through the xdg-desktop-portal, so it also works inside the Flatpak) and puts the
/// chosen path in the dialog: a folder for an export, a file for an import. The chooser blocks, so it runs on a thread.
pub fn browse(ui: &Main) {
    if ui.get_transfer_picking() {
        return;
    }
    ui.set_transfer_picking(true);
    let folder = ui.get_transfer_mode() == 0;
    let typed = expand(ui.get_transfer_path().trim());
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let mut dialog = rfd::FileDialog::new();
        // Start where the typed path points, if that folder exists. No title or filter: the portal's own, translated
        // ones are used, and an Insomnia export may end in .yaml, .yml or nothing.
        let start = if folder && typed.is_dir() {
            Some(typed.as_path())
        } else {
            typed.parent().filter(|d| d.is_dir())
        };
        if let Some(dir) = start {
            dialog = dialog.set_directory(dir);
        }
        let picked = if folder {
            dialog.pick_folder()
        } else {
            dialog.pick_file()
        };
        let _ = slint::invoke_from_event_loop(move || {
            let ui = weak.unwrap();
            ui.set_transfer_picking(false);
            if let Some(path) = picked {
                ui.set_transfer_path(path.display().to_string().into());
                // The message of an earlier attempt no longer describes this choice.
                ui.set_transfer_kind(-1);
            }
        });
    });
}

fn report(ui: &Main, kind: i32, detail: &str) {
    ui.set_transfer_kind(kind);
    ui.set_transfer_detail(detail.into());
}

pub fn run(ui: &Main, c: &Ctx) {
    let exporting = ui.get_transfer_mode() == 0;
    let typed = ui.get_transfer_path();
    if typed.trim().is_empty() {
        // 15: no folder, 3: no file.
        return report(ui, if exporting { 15 } else { 3 }, "");
    }
    let path = expand(typed.trim());
    // What the editor holds is part of the project, whichever way the file goes.
    flush(ui, c);
    if exporting {
        export(ui, c, &path);
    } else {
        import(ui, c, &path, ui.get_transfer_format() == 1);
    }
}

/// The file of each name in `dir`, in the same order; `Err(file name)` for the first one two projects would share
/// (they are told apart only by name, and a clash would make the second overwrite the first).
fn plan(dir: &Path, names: &[&str]) -> Result<Vec<PathBuf>, String> {
    let mut seen = HashSet::new();
    names
        .iter()
        .map(|name| {
            let file = format!("{}.fastapiclient.json", file_stem(name));
            if seen.insert(file.clone()) {
                Ok(dir.join(file))
            } else {
                Err(file)
            }
        })
        .collect()
}

enum Failure {
    /// A file of the export is already there (its name); nothing was written.
    Exists(String),
    Io(String),
}

/// Writes every file or none. An existing file is never replaced, and 0600 because a full export holds secrets.
fn write_all(files: &[(PathBuf, String)]) -> Result<(), Failure> {
    let name = |p: &Path| {
        p.file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
    };
    if let Some((path, _)) = files.iter().find(|(p, _)| p.exists()) {
        return Err(Failure::Exists(name(path)));
    }
    let mut created: Vec<&Path> = vec![];
    for (path, text) in files {
        // `create_new` again: the file may have appeared since the check above.
        let written = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .and_then(|mut f| {
                created.push(path);
                f.write_all(text.as_bytes())
            });
        if let Err(e) = written {
            // Do not leave half an export behind.
            for done in &created {
                let _ = fs::remove_file(done);
            }
            return Err(match e.kind() {
                ErrorKind::AlreadyExists => Failure::Exists(name(path)),
                _ => Failure::Io(e.to_string()),
            });
        }
    }
    Ok(())
}

/// One file per ticked project, in the folder `dir`.
fn export(ui: &Main, c: &Ctx, dir: &Path) {
    let include = ui.get_transfer_include();
    let files: Vec<(PathBuf, String)> = {
        let app = c.app.borrow();
        let ticked: Vec<&Project> = app
            .ws
            .projects
            .iter()
            .zip(c.export_rows.iter())
            .filter(|(_, row)| row.checked)
            .map(|(project, _)| project)
            .collect();
        let names: Vec<&str> = ticked.iter().map(|p| p.name.as_str()).collect();
        match plan(dir, &names) {
            Ok(paths) => paths
                .into_iter()
                .zip(ticked.iter().map(|p| exchange::export(p, include)))
                .collect(),
            Err(file) => return report(ui, 14, &file),
        }
    };
    if files.is_empty() {
        return;
    }
    match write_all(&files) {
        Err(Failure::Exists(file)) => report(ui, 14, &file),
        Err(Failure::Io(e)) => report(ui, 5, &e),
        Ok(()) if files.len() == 1 => report(ui, 1, &files[0].0.display().to_string()),
        Ok(()) => {
            ui.set_transfer_count(files.len() as i32);
            report(ui, 13, &dir.display().to_string());
        }
    }
}

/// `from_insomnia`: the file is an Insomnia v5 export instead of a FastApiClient project.
fn import(ui: &Main, c: &Ctx, path: &PathBuf, from_insomnia: bool) {
    let read = fs::metadata(path).and_then(|m| {
        if m.len() > MAX_IMPORT {
            return Err(std::io::Error::other("file too large"));
        }
        fs::read_to_string(path)
    });
    let text = match read {
        Ok(t) => t,
        Err(e) => return report(ui, 6, &e.to_string()),
    };
    let parsed = if from_insomnia {
        insomnia::import(&text).map(|p| vec![p])
    } else {
        exchange::import(&text)
    };
    match parsed {
        Ok(projects) => {
            let count = projects.len();
            let first = projects.first().map(|p| p.name.clone()).unwrap_or_default();
            {
                let mut app = c.app.borrow_mut();
                for project in projects {
                    let i = app.ws.import_project(project);
                    if app.persist {
                        save(&app.ws.projects[i]);
                    }
                }
            }
            refresh(ui, c, true);
            // One project is named; several are counted (a file always holds at least one).
            if count == 1 {
                report(ui, 2, &first);
            } else {
                report(ui, 12, &count.to_string());
            }
        }
        Err(ImportError::Invalid(d)) => report(ui, if from_insomnia { 11 } else { 8 }, &d),
        Err(ImportError::NotAProject) => report(ui, if from_insomnia { 10 } else { 7 }, ""),
        Err(ImportError::TooNew(_)) => report(ui, 9, ""),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use fastapiclient_core::model::new_id;

    use super::*;

    #[test]
    fn suggested_file_names_are_safe() {
        assert_eq!(file_stem("Payments v2"), "Payments_v2");
        assert_eq!(file_stem("../../etc"), "______etc");
        assert_eq!(file_stem("///"), "project");
        assert_eq!(file_stem(""), "project");
    }

    #[test]
    fn only_a_leading_tilde_is_expanded() {
        assert_eq!(expand("/tmp/a~/b"), PathBuf::from("/tmp/a~/b"));
        assert_eq!(expand("relative.json"), PathBuf::from("relative.json"));
    }

    #[test]
    fn each_project_gets_its_own_file_and_a_clash_is_reported() {
        let dir = Path::new("/out");
        let paths = plan(dir, &["Shop", "Pay ments"]).unwrap();
        assert_eq!(
            paths,
            [
                PathBuf::from("/out/Shop.fastapiclient.json"),
                PathBuf::from("/out/Pay_ments.fastapiclient.json")
            ]
        );
        // "A/B" and "A_B" both become A_B: the second would replace the first.
        assert_eq!(plan(dir, &["A/B", "A_B"]), Err("A_B.fastapiclient.json".to_string()));
    }

    #[test]
    fn an_export_writes_every_file_or_none() {
        let dir = std::env::temp_dir().join(format!("fac-export-{}", new_id()));
        fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a.json"), dir.join("b.json"));
        let files = [(a.clone(), "one".to_string()), (b.clone(), "two".to_string())];
        assert!(write_all(&files).is_ok());
        assert_eq!(fs::read_to_string(&b).unwrap(), "two");
        assert_eq!(fs::metadata(&a).unwrap().permissions().mode() & 0o777, 0o600);

        // `a` exists now: a new file listed before it must not be written either, and `a` keeps its content.
        let c = dir.join("c.json");
        let again = [(c.clone(), "new".to_string()), (a.clone(), "changed".to_string())];
        assert!(matches!(write_all(&again), Err(Failure::Exists(n)) if n == "a.json"));
        assert!(!c.exists());
        assert_eq!(fs::read_to_string(&a).unwrap(), "one");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_missing_folder_is_an_io_error_and_leaves_nothing() {
        let dir = std::env::temp_dir().join(format!("fac-missing-{}", new_id()));
        let files = [(dir.join("a.json"), "x".to_string())];
        assert!(matches!(write_all(&files), Err(Failure::Io(_))));
    }
}
