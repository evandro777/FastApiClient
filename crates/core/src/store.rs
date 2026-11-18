//! One JSON file per project under `<data dir>/projects/`.
use std::{
    fs, io,
    path::{Path, PathBuf},
};

use crate::{
    files::{backup_path, create_private_dir, write_atomic},
    model::Project,
    settings::data_dir,
};

pub fn projects_dir() -> PathBuf {
    data_dir().join("projects")
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

pub fn save(project: &Project) -> io::Result<()> {
    save_in(&projects_dir(), project)
}

pub fn load_all() -> Vec<Project> {
    load_all_in(&projects_dir())
}

pub fn delete(id: &str) -> io::Result<()> {
    delete_in(&projects_dir(), id)
}

fn save_in(dir: &Path, project: &Project) -> io::Result<()> {
    // The id becomes a file name: refuse anything that could leave `dir`.
    if !valid_id(&project.id) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid project id"));
    }
    let text = serde_json::to_string_pretty(project).map_err(io::Error::other)?;
    write_atomic(&dir.join(format!("{}.json", project.id)), &text)
}

/// Sorted by name. An unreadable file is renamed to `.bad` (never overwritten, never silently dropped) and skipped.
fn load_all_in(dir: &Path) -> Vec<Project> {
    // Projects hold tokens and passwords: a folder made by an older version (0755) is closed to other users now.
    let _ = create_private_dir(dir);
    let Ok(entries) = fs::read_dir(dir) else { return vec![] };
    remove_dead_temp_files(dir);
    let mut out: Vec<Project> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .filter_map(|path| {
            let parsed = fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|t| serde_json::from_str::<Project>(&t).map_err(|e| e.to_string()))
                // The file name is the source of truth, so a hand-edited `id` cannot point elsewhere.
                .map(|mut p| {
                    p.id = path
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    p.normalize_environments();
                    p
                })
                .and_then(|p| {
                    if valid_id(&p.id) {
                        Ok(p)
                    } else {
                        Err("invalid id".into())
                    }
                });
            parsed
                .inspect_err(|e| {
                    let bad = backup_path(&path);
                    eprintln!("{} is invalid ({e}); kept as {}", path.display(), bad.display());
                    let _ = fs::rename(&path, bad);
                })
                .ok()
        })
        .collect();
    out.sort_by_key(|p| p.name.to_lowercase());
    out
}

/// `<name>.<pid>.tmp` is left by a process that died between write and rename; one whose process still runs is a
/// write in progress and stays.
fn remove_dead_temp_files(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for path in entries.flatten().map(|e| e.path()) {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        let pid = name
            .strip_suffix(".tmp")
            .and_then(|n| n.rsplit('.').next()?.parse::<u32>().ok());
        if pid.is_some_and(|pid| !Path::new(&format!("/proc/{pid}")).exists()) {
            let _ = fs::remove_file(&path);
        }
    }
}

fn delete_in(dir: &Path, id: &str) -> io::Result<()> {
    if !valid_id(id) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid project id"));
    }
    match fs::remove_file(dir.join(format!("{id}.json"))) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fac-store-{}", crate::model::new_id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn save_load_delete_roundtrip() {
        let dir = temp_dir();
        let (b, a) = (Project::new("beta"), Project::new("Alpha"));
        save_in(&dir, &b).unwrap();
        save_in(&dir, &a).unwrap();
        let names: Vec<_> = load_all_in(&dir).into_iter().map(|p| p.name).collect();
        assert_eq!(names, ["Alpha", "beta"]);
        delete_in(&dir, &a.id).unwrap();
        delete_in(&dir, &a.id).unwrap(); // Already absent is still deleted.
        assert_eq!(load_all_in(&dir).len(), 1);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn loaded_environment_names_are_repaired_without_changing_the_active_values() {
        let dir = temp_dir();
        let mut p = Project::new("Names");
        p.environments = ["Shared", "Shared", ""]
            .iter()
            .enumerate()
            .map(|(i, name)| crate::model::Environment {
                name: (*name).into(),
                values: serde_json::json!({"id": i}),
            })
            .collect();
        p.active_env = "Shared".into();
        save_in(&dir, &p).unwrap();
        let loaded = load_all_in(&dir);
        assert_eq!(
            loaded[0]
                .environments
                .iter()
                .map(|e| e.name.as_str())
                .collect::<Vec<_>>(),
            ["Shared", "Shared 2", "3"]
        );
        assert_eq!(loaded[0].active_values()["id"], 0);
        assert_eq!(loaded[0].environments.len(), 3);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn bad_file_is_kept_aside_and_does_not_block_others() {
        let dir = temp_dir();
        save_in(&dir, &Project::new("ok")).unwrap();
        fs::write(dir.join("broken.json"), "{not json").unwrap();
        assert_eq!(load_all_in(&dir).len(), 1);
        let kept = |dir: &Path| {
            fs::read_dir(dir)
                .unwrap()
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with("broken.json.bad."))
                .count()
        };
        assert_eq!(kept(&dir), 1);
        fs::write(dir.join("broken.json"), "{still not json").unwrap();
        load_all_in(&dir);
        assert_eq!(kept(&dir), 2, "a second bad file does not replace the first");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn projects_are_private_even_in_a_folder_made_by_an_older_version() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        load_all_in(&dir);
        assert_eq!(mode(&dir), 0o700);
        let p = Project::new("secret");
        save_in(&dir, &p).unwrap();
        assert_eq!(mode(&dir.join(format!("{}.json", p.id))), 0o600);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn temp_files_of_dead_processes_are_removed_and_live_ones_kept() {
        let dir = temp_dir();
        let dead = dir.join("a.json.4294967294.tmp");
        let live = dir.join(format!("b.json.{}.tmp", std::process::id()));
        for f in [&dead, &live] {
            fs::write(f, "x").unwrap();
        }
        load_all_in(&dir);
        assert!(!dead.exists() && live.exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn ids_that_could_escape_the_directory_are_refused() {
        let dir = temp_dir();
        let evil = Project {
            id: "../x".into(),
            ..Project::default()
        };
        assert!(save_in(&dir, &evil).is_err());
        assert!(delete_in(&dir, "../x").is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}
