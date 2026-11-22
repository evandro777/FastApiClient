//! "Open in the default app": the response goes to a private file that `xdg-open` hands to the desktop.

/// Extension guessed from the text, so the default editor picks a syntax. Sniffing avoids carrying the Content-Type here.
/// Only a real XML declaration gets `xml`: any other markup (HTML, SVG, scripts) stays text, so a server's page never
/// opens in a browser where its scripts would run.
fn guess_ext(text: &str) -> &'static str {
    let t = text.trim_start();
    if t.starts_with(['{', '[']) {
        "json"
    } else if t.starts_with("<?xml") {
        "xml"
    } else {
        "txt"
    }
}

/// Under the cache dir, not /tmp, which a Flatpak sandbox does not share with the host.
fn response_dir() -> std::io::Result<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache")))
        .ok_or_else(|| std::io::Error::other("no cache directory"))?;
    Ok(base.join("FastApiClient"))
}

/// A response can hold a token: the copy opened in an external editor does not outlive the next start.
pub fn remove_old_responses() {
    if let Ok(dir) = response_dir() {
        remove_old_in(&dir);
    }
}

fn remove_old_in(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for path in entries.flatten().map(|e| e.path()) {
        if path.file_stem().is_some_and(|s| s == "response") {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// One fixed file there, overwritten each time.
pub fn open_external(text: &str) -> std::io::Result<()> {
    let dir = response_dir()?;
    // A response can hold a token, so the file is private like the projects.
    fastapiclient_core::files::create_private_dir(&dir)?;
    let file = dir.join(format!("response.{}", guess_ext(text)));
    fastapiclient_core::files::write_private(&file, text)?;
    let mut child = std::process::Command::new("xdg-open").arg(file).spawn()?;
    // Reaped on a thread, or every call would leave a zombie in this long-lived process.
    std::thread::spawn(move || _ = child.wait());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::guess_ext;

    #[test]
    fn old_responses_are_removed_and_other_files_stay() {
        let app = std::env::temp_dir().join(format!("fac-external-{}", std::process::id()));
        std::fs::create_dir_all(&app).unwrap();
        for name in ["response.json", "response.txt", "notes.txt"] {
            std::fs::write(app.join(name), "x").unwrap();
        }
        super::remove_old_in(&app);
        let mut left: Vec<_> = std::fs::read_dir(&app)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        left.sort();
        assert_eq!(left, ["notes.txt"]);
        std::fs::remove_dir_all(app).unwrap();
    }

    #[test]
    fn guesses_extension() {
        assert_eq!(guess_ext(" {\"a\":1}"), "json");
        assert_eq!(guess_ext("[1]"), "json");
        assert_eq!(guess_ext("<!DOCTYPE html><p>"), "txt");
        assert_eq!(guess_ext("<svg onload=x>"), "txt");
        assert_eq!(guess_ext("<?xml version=\"1.0\"?>"), "xml");
        assert_eq!(guess_ext("hello"), "txt");
    }
}
