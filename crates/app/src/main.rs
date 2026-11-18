use fastapiclient_core::{
    chain::{self, Responses},
    http::{self, Error, Response},
    model::{Body, EnvError, Environment, Folder, Project},
    settings::Settings,
    store,
    workspace::{Deleted, Kind, Node, Sel, Workspace},
};
use slint::{Model, ModelRc, SharedString, VecModel};
use std::{
    cell::RefCell,
    collections::HashSet,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

slint::include_modules!();

#[cfg(test)]
mod app_tests;
mod editor;
mod fixtures;
mod highlight;
mod instance;
mod payload;
mod sending;
mod transfer;
mod validation;

use editor::{Methods, ensure_blank};

/// The three key/value tables of the editor: query, headers, form body (the order `kv-edited` uses).
type Tables = [Rc<VecModel<KeyValueRow>>; 3];

struct App {
    ws: Workspace,
    /// False with fixtures, so a snapshot run never writes to the real data directory.
    persist: bool,
    dirty: HashSet<String>,
}

/// Everything the callbacks share. Cloning is cheap: it only clones the handles.
#[derive(Clone)]
struct Ctx {
    app: Rc<RefCell<App>>,
    tables: Tables,
    methods: Methods,
    tree: Rc<VecModel<TreeRow>>,
    /// The projects of the export dialog, with their checkboxes.
    export_rows: Rc<VecModel<ExportRow>>,
    /// Bumped whenever the selection changes, so a slow response is not shown under another request.
    generation: Arc<AtomicU64>,
    /// Responses that `{{ $response(...) }}` reads, shared with the sending threads.
    responses: Arc<Responses>,
    dialogs: Arc<sending::Dialogs>,
}

fn main() {
    if std::env::args().skip(1).any(|a| a == "-V" || a == "--version") {
        return println!("fastapiclient {}", env!("CARGO_PKG_VERSION"));
    }
    // Fixtures (scripts/ui-snapshot.sh) never touch a real running instance.
    let fixture_run = fixtures::mode().is_some();
    // One window: a copy is already open, so it comes to the front and this one leaves before setting anything up.
    if !fixture_run && instance::send("show") {
        return;
    }
    // Forced so the 100 MB GL renderer is never pulled in by accident.
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("software".into())
        .select()
        .expect("cannot start the Slint software renderer");

    let settings = Settings::load().unwrap_or_else(|e| {
        eprintln!("cannot load settings safely: {e}");
        std::process::exit(1);
    });
    let fixture_mode = fixtures::mode();
    if !fixture_run {
        remove_old_responses();
    }
    let (ws, persist) = match fixture_mode {
        // With fixtures nothing is read, saved or sent, so the real system is never touched.
        Some(mode) => (fixtures::workspace(mode), false),
        None => (load_workspace(&settings), true),
    };
    let ui = Main::new().unwrap();
    // Take the socket now that the window exists; a later start sends "show" and this window is raised.
    if !fixture_run {
        let weak = ui.as_weak();
        let raise = move |_command: String| {
            let _ = weak.upgrade_in_event_loop(|ui| instance::raise(ui.window()));
        };
        if instance::claim("show", raise) {
            return;
        }
    }
    ui.set_auto_beautify(settings.auto_beautify);
    ui.set_sidebar_width(settings.sidebar_width);
    ui.set_request_split(settings.request_split);
    ui.set_color_scheme_index(theme_index(&settings.theme));
    if fixture_run && let Ok(theme) = std::env::var("FASTAPICLIENT_THEME") {
        ui.set_color_scheme_index(theme_index(&theme));
    }
    ui.set_language_index(LANGUAGES.iter().position(|l| *l == settings.language).unwrap_or(0) as i32);
    // One model per table for the whole run: the callbacks must all push into the same ones.
    let c = Ctx {
        app: Rc::new(RefCell::new(App {
            ws,
            persist,
            dirty: HashSet::new(),
        })),
        tables: std::array::from_fn(|_| Rc::new(VecModel::default())),
        methods: Rc::new(VecModel::default()),
        tree: Rc::new(VecModel::default()),
        export_rows: Rc::new(VecModel::default()),
        generation: Arc::new(AtomicU64::new(0)),
        responses: Arc::default(),
        dialogs: Arc::default(),
    };
    ui.set_tree_rows(c.tree.clone().into());
    ui.set_export_rows(c.export_rows.clone().into());
    ui.set_methods(c.methods.clone().into());
    ui.set_query_rows(c.tables[0].clone().into());
    ui.set_header_rows(c.tables[1].clone().into());
    ui.set_form_rows(c.tables[2].clone().into());
    // Only fails when no translation matches; the source strings are English anyway.
    let _ = slint::select_bundled_translation(&language(&settings));
    wire(&ui, &c, settings);
    highlight::wire(&ui);

    refresh(&ui, &c, true);
    if let Some(mode) = fixture_mode {
        fixtures::response(&ui, mode);
    }
    ui.set_version(env!("CARGO_PKG_VERSION").into());
    // Opens a tab (or the About overlay) directly, for the snapshot script.
    match std::env::var("FASTAPICLIENT_PAGE").as_deref() {
        Ok("about") => ui.set_about_open(true),
        Ok("menu") => ui.set_menu_open(true),
        Ok("save-error") if fixture_run => {
            ui.set_save_project("Payments".into());
            ui.set_save_error("Permission denied (os error 13)".into());
        }
        Ok("cache-limit") if fixture_run => {
            let weak = ui.as_weak();
            slint::Timer::single_shot(Duration::from_millis(200), move || {
                let ui = weak.unwrap();
                highlight::clear_response(&ui);
                show_error(&ui, Error::CacheLimit);
            });
        }
        Ok("delete-error") if fixture_run => {
            let weak = ui.as_weak();
            // The initial selection reload clears errors before the first frame.
            slint::Timer::single_shot(Duration::from_millis(200), move || {
                let ui = weak.unwrap();
                ui.set_failed_delete_project("Payments".into());
                ui.set_delete_error("Permission denied (os error 13)".into());
            });
        }
        Ok("response-xml") if fixture_run && ui.get_has_response() => {
            ui.set_search_open(false);
            ui.set_auto_beautify(true);
            let text = "<root><name> Ana </name><blank> </blank></root>";
            ui.set_truncated(false);
            ui.set_size_bytes(text.len() as i32);
            highlight::response(&ui, text, "application/xml");
        }
        Ok("filter" | "filter-none") if fixture_run => {
            ui.set_request_filter(
                if std::env::var("FASTAPICLIENT_PAGE").as_deref() == Ok("filter") {
                    "us"
                } else {
                    "no-such-request"
                }
                .into(),
            );
            refresh(&ui, &c, false);
        }
        Ok("prompt") if fixture_run => fixtures::prompt(&ui),
        Ok("new-folder") if fixture_run => ui.invoke_ask_create(true),
        Ok("new-request") if fixture_run => ui.invoke_ask_create(false),
        Ok("folder-menu") if fixture_run => {
            let weak = ui.as_weak();
            slint::Timer::single_shot(Duration::from_millis(200), move || {
                let ui = weak.unwrap();
                ui.invoke_select(0, 0, -1);
                ui.set_preview_folder_menu(true);
            });
        }
        Ok("response-raw") if fixture_run => {
            ui.set_response_mode(0);
            ui.invoke_response_view(false);
        }
        Ok("response-normal") if fixture_run => ui.set_search_open(false),
        Ok("response-find") if fixture_run => ui.set_search_open(true),
        Ok("response-notice") if fixture_run && ui.get_has_response() => {
            ui.set_search_open(false);
            highlight::response(&ui, "{\"unfinished\":", "application/json");
        }
        // The list needs a shown window, so it opens a moment after the first frame.
        Ok("vars") => {
            let weak = ui.as_weak();
            slint::Timer::single_shot(std::time::Duration::from_millis(400), move || {
                weak.unwrap().set_demo_vars(true)
            });
        }
        Ok("manage") => ui.set_manage_open(true),
        Ok(
            page @ ("validation-json" | "validation-xml" | "validation-dtd" | "payload-errors" | "payload-lines"
            | "payload-empty" | "highlight-json" | "highlight-xml" | "highlight-limit" | "validation-limit"),
        ) if fixture_run => {
            fixtures::payload(&ui, fixture_mode.unwrap_or(0), page);
        }
        // Canned text only: the real dialog is opened by Send on a request with a file body.
        Ok("sendfile") => {
            ui.set_send_file_path("/home/user/Documents/report.pdf".into());
            ui.set_send_file_host("api.example.com".into());
            ui.set_prompt_kind(8);
            ui.set_prompt_open(true);
        }
        Ok("envs") => {
            open_envs(&ui, &c);
            ui.set_envs_open(true);
            if fixture_mode == Some(3) {
                fixtures::env_error(&ui);
            }
        }
        Ok(p @ ("export" | "import" | "insomnia" | "import-headers")) => {
            // 0 export, 1 import, 2 import with the Insomnia format picked.
            let dialog = match p {
                "export" => 0,
                "import" => 1,
                _ => 2,
            };
            transfer::prepare(&ui, &c, dialog.min(1));
            ui.set_transfer_format(i32::from(dialog == 2));
            ui.set_transfer_open(true);
            if let Some(mode) = fixture_mode {
                fixtures::transfer(&ui, mode, dialog);
            }
            if p == "import-headers" {
                ui.set_transfer_kind(16);
            }
        }
        Ok(p) => ui.set_tab(p.parse().unwrap_or(0)),
        Err(_) => {}
    }
    // Edits are copied into the model and saved here, so a crash loses at most a second of typing.
    let autosave = slint::Timer::default();
    autosave.start(slint::TimerMode::Repeated, Duration::from_secs(1), {
        let (weak, c) = (ui.as_weak(), c.clone());
        move || {
            if let Some(ui) = weak.upgrade() {
                flush(&ui, &c);
            }
        }
    });
    ui.run().unwrap();
}

fn load_workspace(settings: &Settings) -> Workspace {
    let mut ws = Workspace::new(store::load_all());
    if let Some(project) = ws.projects.iter().position(|p| p.id == settings.last_project) {
        let wanted = Sel {
            project,
            folder: settings.last_folder,
            request: settings.last_request,
        };
        // A selection that no longer exists (a hand-edited file) falls back to the project.
        if !ws.select(wanted) {
            ws.select(Sel {
                folder: None,
                request: None,
                ..wanted
            });
        }
    }
    ws
}

fn retry_saves(ui: &Main, app: &mut App, mut write: impl FnMut(&Project) -> std::io::Result<()>) -> bool {
    if app.dirty.is_empty() {
        return true;
    }
    let mut error = None;
    app.dirty.retain(|id| {
        let Some(project) = app.ws.projects.iter().find(|p| &p.id == id) else {
            return false;
        };
        match write(project) {
            Ok(()) => false,
            Err(e) => {
                error.get_or_insert_with(|| (project.name.clone(), e.to_string()));
                true
            }
        }
    });
    let (project, detail) = error.unwrap_or_default();
    ui.set_save_project(project.into());
    ui.set_save_error(detail.into());
    app.dirty.is_empty()
}

fn save(ui: &Main, app: &mut App, project: usize) -> bool {
    if app.persist {
        app.dirty.insert(app.ws.projects[project].id.clone());
    }
    retry_saves(ui, app, store::save)
}

/// Copies the editor into the selected node and saves its project if that changed anything.
fn flush(ui: &Main, c: &Ctx) {
    let mut app = c.app.borrow_mut();
    let selected = app.ws.sel;
    let changed = app
        .ws
        .current()
        .is_some_and(|node| editor::store(ui, &c.tables, &c.methods, node));
    if changed
        && app.persist
        && let Some(sel) = selected
    {
        let id = app.ws.projects[sel.project].id.clone();
        app.dirty.insert(id);
    }
    retry_saves(ui, &mut app, store::save);
}

/// Keep the selection and its draft intact unless the saved project has actually gone.
fn delete_project(ui: &Main, app: &mut App, c: &Ctx) -> bool {
    let Some(selected) = app.ws.sel else { return false };
    let project = &app.ws.projects[selected.project];
    if app.persist
        && let Err(e) = store::delete(&project.id)
    {
        ui.set_failed_delete_project(project.name.clone().into());
        ui.set_delete_error(e.to_string().into());
        return false;
    }
    c.generation.fetch_add(1, Ordering::SeqCst);
    c.dialogs.cancel();
    c.responses.remove_project(&project.id);
    app.dirty.remove(&project.id);
    app.ws.select_project(selected.project);
    app.ws.delete();
    ui.set_delete_error("".into());
    true
}

fn kind_number(k: Kind) -> i32 {
    match k {
        Kind::Project => 0,
        Kind::Folder => 1,
        Kind::Request => 2,
    }
}

fn index(i: Option<usize>) -> i32 {
    i.map_or(-1, |i| i as i32)
}

/// Rebuilds the sidebar and the selection properties; with `reload`, also puts the selected node in the editor and
/// forgets the last response (it belonged to another request).
fn refresh(ui: &Main, c: &Ctx, reload: bool) {
    let mut app = c.app.borrow_mut();
    let project_id = app
        .ws
        .sel
        .and_then(|s| app.ws.projects.get(s.project))
        .map(|p| p.id.as_str())
        .unwrap_or("");
    if project_id != ui.get_filter_project_id().as_str() {
        ui.set_request_filter("".into());
        ui.set_filter_project_id(project_id.into());
    }
    let rows: Vec<TreeRow> = app
        .ws
        .filtered_tree(&ui.get_request_filter())
        .into_iter()
        .map(|t| TreeRow {
            text: t.text.into(),
            depth: t.depth.into(),
            kind: kind_number(t.kind),
            method: t.method.into(),
            selected: t.selected,
            project: t.sel.project as i32,
            folder: index(t.sel.folder),
            request: index(t.sel.request),
        })
        .collect();
    let kind = app.ws.sel.map_or(-1, |s| kind_number(s.kind()));
    ui.set_selected_kind(kind);
    ui.set_selected_name(app.ws.name().unwrap_or("").into());
    if reload {
        ui.set_delete_error("".into());
        // Whatever was opened starts on its first tab: Params for a request, Auth for a folder or project (the
        // only tabs they have are Headers and Auth). A request without params but with a body opens on Body.
        let mut tab = if kind == 2 { 0 } else { 3 };
        if let Some(node) = app.ws.current() {
            if let Node::Request(r) = &node
                && !r.query.iter().any(|kv| !kv.key.is_empty())
                && r.body != Body::None
            {
                tab = 2;
            }
            editor::load(ui, &c.tables, &c.methods, node);
        }
        ui.set_tab(tab);
        reset_send(ui, c);
    }
    refresh_envs(ui, &app.ws);
    ui.set_project_options(list(app.ws.project_names()));
    ui.set_project_index(app.ws.sel.map_or(0, |s| s.project as i32));
    ui.set_tree_selected(rows.iter().position(|r| r.selected).map_or(-1, |i| i as i32));
    drop(app);
    sync_tree(&c.tree, rows);
}

/// Updates the rows in place instead of replacing them all: a reset would destroy the row under the pointer, and
/// with it the context menu that a right click has just opened on it.
fn sync_tree(model: &VecModel<TreeRow>, rows: Vec<TreeRow>) {
    for (i, row) in rows.iter().enumerate() {
        match model.row_data(i) {
            Some(old) if old == *row => {}
            Some(_) => model.set_row_data(i, row.clone()),
            None => model.push(row.clone()),
        }
    }
    while model.row_count() > rows.len() {
        model.remove(model.row_count() - 1);
    }
}

/// The model of a drop-down list. A new one each time, never `set_vec` on a shared one: a ComboBox keeps its own copy
/// of the selected text, and only a new model makes it read that text again. With the same model, a rename (or a
/// delete that leaves the same index) kept showing the old name.
fn list(items: impl IntoIterator<Item = impl Into<SharedString>>) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(
        items.into_iter().map(Into::into).collect::<Vec<SharedString>>(),
    ))
}

/// The environment lists follow the selected project.
fn refresh_envs(ui: &Main, ws: &Workspace) {
    let names = ws.env_names();
    let mut options = vec![ui.global::<Tr>().invoke_no_environment()];
    options.extend(names.iter().map(|n| SharedString::from(n.as_str())));
    ui.set_env_options(list(options));
    ui.set_env_names(list(names.iter().cloned()));
    ui.set_env_active_index(ws.active_env_index().map_or(0, |i| i as i32 + 1));
    set_var_names(ui, ws);
    ui.set_env_edit_index(ui.get_env_edit_index().clamp(0, names.len().saturating_sub(1) as i32));
}

fn set_var_names(ui: &Main, ws: &Workspace) {
    ui.set_var_names(list(ws.var_names()));
}

fn load_env_text(ui: &Main, ws: &Workspace) {
    let text = ws.env_text(ui.get_env_edit_index().max(0) as usize);
    ui.set_env_text(text.unwrap_or_default().into());
    ui.set_env_error_kind(-1);
    ui.set_env_dirty(false);
}

fn show_env_error(ui: &Main, e: EnvError) {
    match e {
        EnvError::Invalid(detail) => {
            ui.set_env_error_kind(1);
            ui.set_env_error_detail(detail.into());
        }
        EnvError::NotObject => ui.set_env_error_kind(2),
    }
}

/// Prepares the environment editor: it opens on the active environment.
/// Extension guessed from the text, so the default editor picks a syntax. Sniffing avoids carrying the Content-Type here.
fn guess_ext(text: &str) -> &'static str {
    let t = text.trim_start();
    let lower: String = t.chars().take(64).collect::<String>().to_ascii_lowercase();
    if t.starts_with(['{', '[']) {
        "json"
    } else if t.starts_with('<') && !lower.starts_with("<!doctype html") && !lower.starts_with("<html") {
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
fn remove_old_responses() {
    let Ok(entries) = response_dir().and_then(std::fs::read_dir) else {
        return;
    };
    for path in entries.flatten().map(|e| e.path()) {
        if path.file_stem().is_some_and(|s| s == "response") {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// One fixed file there, overwritten each time.
fn open_external(text: &str) -> std::io::Result<()> {
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

fn open_envs(ui: &Main, c: &Ctx) {
    let app = c.app.borrow();
    ui.set_env_edit_index(app.ws.active_env_index().map_or(0, |i| i as i32));
    load_env_text(ui, &app.ws);
}

fn reset_send(ui: &Main, c: &Ctx) {
    c.generation.fetch_add(1, Ordering::SeqCst);
    c.dialogs.cancel();
    if matches!(ui.get_prompt_kind(), 8 | 9) {
        ui.set_prompt_open(false);
    }
    ui.set_sending(false);
    highlight::clear_response(ui);
    ui.set_request_log("".into());
    ui.set_error_kind(-1);
    ui.set_warning_vars("".into());
    ui.set_truncated(false);
}

/// Refresh environment lists without reloading the request's unsaved editor values.
fn env_op(ui: &Main, c: &Ctx, op: impl FnOnce(&mut Workspace, usize) -> Option<usize>) {
    let edit = ui.get_env_edit_index().max(0) as usize;
    let changed = {
        let mut app = c.app.borrow_mut();
        let project = op(&mut app.ws, edit);
        if let Some(i) = project.filter(|_| app.persist) {
            save(ui, &mut app, i);
        }
        project.is_some()
    };
    if changed {
        reset_send(ui, c);
    }
    refresh_envs(ui, &c.app.borrow().ws);
}

fn wire(ui: &Main, c: &Ctx, settings: Settings) {
    ui.on_filter_edited({
        let (weak, c) = (ui.as_weak(), c.clone());
        move || refresh(&weak.unwrap(), &c, false)
    });

    ui.on_select({
        let (weak, c) = (ui.as_weak(), c.clone());
        move |project, folder, request| {
            let ui = weak.unwrap();
            let sel = Sel {
                project: project as usize,
                folder: usize::try_from(folder).ok(),
                request: usize::try_from(request).ok(),
            };
            if c.app.borrow().ws.sel == Some(sel) {
                return;
            }
            flush(&ui, &c);
            if c.app.borrow_mut().ws.select(sel) {
                refresh(&ui, &c, true);
            }
        }
    });
    // Up/Down in the tree: the row above or below the selected one, in the order shown.
    ui.on_step({
        let (weak, c) = (ui.as_weak(), c.clone());
        move |delta| {
            let ui = weak.unwrap();
            let target = {
                let app = c.app.borrow();
                let tree = app.ws.filtered_tree(&ui.get_request_filter());
                match tree.iter().position(|t| t.selected) {
                    // With the project itself open (no row highlighted), Down goes to the first row.
                    None => tree.first().filter(|_| delta > 0).map(|t| t.sel),
                    Some(at) => usize::try_from(at as i32 + delta)
                        .ok()
                        .and_then(|i| tree.get(i))
                        .map(|t| t.sel),
                }
            };
            let Some(sel) = target else { return };
            flush(&ui, &c);
            if c.app.borrow_mut().ws.select(sel) {
                refresh(&ui, &c, true);
            }
        }
    });
    ui.on_project_activated({
        let (weak, c) = (ui.as_weak(), c.clone());
        move |index| {
            let ui = weak.unwrap();
            if c.app.borrow().ws.sel.map(|s| s.project as i32) == Some(index) {
                return;
            }
            flush(&ui, &c);
            if c.app.borrow_mut().ws.select_project(index.max(0) as usize) {
                ui.set_request_filter("".into());
                refresh(&ui, &c, true);
            }
        }
    });
    ui.on_new_project({
        let (weak, c) = (ui.as_weak(), c.clone());
        move |name| {
            // The prompt only creates on Enter or Create; an empty name creates nothing.
            let name = name.trim();
            if name.is_empty() {
                return;
            }
            let ui = weak.unwrap();
            flush(&ui, &c);
            {
                let mut app = c.app.borrow_mut();
                let i = app.ws.add_project(name);
                if app.persist {
                    save(&ui, &mut app, i);
                }
            }
            refresh(&ui, &c, true);
        }
    });
    ui.on_new_folder({
        let (weak, c) = (ui.as_weak(), c.clone());
        move |name| {
            let name = name.trim();
            if name.is_empty() {
                return;
            }
            let ui = weak.unwrap();
            flush(&ui, &c);
            {
                let mut app = c.app.borrow_mut();
                if let Some(i) = app.ws.add_folder(name).filter(|_| app.persist) {
                    save(&ui, &mut app, i);
                }
            }
            refresh(&ui, &c, true);
        }
    });
    ui.on_new_request({
        let (weak, c) = (ui.as_weak(), c.clone());
        move |name| {
            let name = name.trim();
            if name.is_empty() {
                return;
            }
            let ui = weak.unwrap();
            flush(&ui, &c);
            {
                let mut app = c.app.borrow_mut();
                if let Some(i) = app.ws.add_request(name).filter(|_| app.persist) {
                    save(&ui, &mut app, i);
                }
            }
            refresh(&ui, &c, true);
        }
    });
    ui.on_rename({
        let (weak, c) = (ui.as_weak(), c.clone());
        move |name| {
            let ui = weak.unwrap();
            flush(&ui, &c);
            {
                let mut app = c.app.borrow_mut();
                if let Some(i) = app.ws.rename(&name).filter(|_| app.persist) {
                    save(&ui, &mut app, i);
                }
            }
            refresh(&ui, &c, false);
        }
    });
    ui.on_rename_project({
        let (weak, c) = (ui.as_weak(), c.clone());
        move |name| {
            let ui = weak.unwrap();
            {
                let mut app = c.app.borrow_mut();
                if let Some(i) = app.ws.rename_project(&name).filter(|_| app.persist) {
                    save(&ui, &mut app, i);
                }
            }
            refresh(&ui, &c, false);
        }
    });
    ui.on_delete_project({
        let (weak, c) = (ui.as_weak(), c.clone());
        move || {
            let ui = weak.unwrap();
            // No flush: the editor holds a node of the project that is about to disappear.
            {
                let mut app = c.app.borrow_mut();
                if !delete_project(&ui, &mut app, &c) {
                    return;
                }
            }
            refresh(&ui, &c, true);
        }
    });
    // Opens the project itself (its shared headers and auth); it has no row in the tree.
    ui.on_global_config({
        let (weak, c) = (ui.as_weak(), c.clone());
        move || {
            let ui = weak.unwrap();
            let Some(project) = c.app.borrow().ws.sel.map(|s| s.project) else {
                return;
            };
            flush(&ui, &c);
            if c.app.borrow_mut().ws.select_project(project) {
                refresh(&ui, &c, true);
            }
        }
    });
    ui.on_delete_selected({
        let (weak, c) = (ui.as_weak(), c.clone());
        move || {
            let ui = weak.unwrap();
            // No flush: the editor holds the node that is about to disappear.
            {
                let mut app = c.app.borrow_mut();
                if app.ws.sel.is_some_and(|s| s.kind() == Kind::Project) {
                    if !delete_project(&ui, &mut app, &c) {
                        return;
                    }
                } else if let Some(Deleted::Part(i)) = app.ws.delete()
                    && app.persist
                {
                    save(&ui, &mut app, i);
                }
            }
            refresh(&ui, &c, true);
        }
    });
    ui.on_env_activated({
        let (weak, c) = (ui.as_weak(), c.clone());
        // Index 0 is "no environment", so the real ones are shifted by one.
        move |i| {
            env_op(&weak.unwrap(), &c, |ws, _| {
                ws.set_active_env(usize::try_from(i - 1).ok())
            })
        }
    });
    ui.on_manage_envs({
        let (weak, c) = (ui.as_weak(), c.clone());
        move || open_envs(&weak.unwrap(), &c)
    });
    ui.on_env_picked({
        let (weak, c) = (ui.as_weak(), c.clone());
        move |i| {
            let ui = weak.unwrap();
            ui.set_env_edit_index(i);
            load_env_text(&ui, &c.app.borrow().ws);
        }
    });
    // Typing only checks the text; nothing is stored until the Save button (`env_save`).
    ui.on_env_edited({
        let weak = ui.as_weak();
        move |text| {
            let ui = weak.unwrap();
            match Environment::parse(&text) {
                Ok(_) => ui.set_env_error_kind(-1),
                Err(e) => show_env_error(&ui, e),
            }
        }
    });
    ui.on_env_save({
        let (weak, c) = (ui.as_weak(), c.clone());
        move |text| {
            let ui = weak.unwrap();
            match Environment::parse(&text) {
                Ok(values) => {
                    env_op(&ui, &c, |ws, edit| ws.set_env_values(edit, values));
                    // Shows the saved value re-indented, and clears the error and the unsaved mark.
                    load_env_text(&ui, &c.app.borrow().ws);
                }
                Err(e) => show_env_error(&ui, e),
            }
        }
    });
    ui.on_env_add({
        let (weak, c) = (ui.as_weak(), c.clone());
        move |name| {
            let ui = weak.unwrap();
            env_op(&ui, &c, |ws, _| ws.add_env(&name));
            // The new one is the last, and the active one.
            ui.set_env_edit_index(ui.get_env_names().row_count() as i32 - 1);
            load_env_text(&ui, &c.app.borrow().ws);
        }
    });
    ui.on_env_rename({
        let (weak, c) = (ui.as_weak(), c.clone());
        move |name| env_op(&weak.unwrap(), &c, |ws, i| ws.rename_env(i, &name))
    });
    ui.on_env_delete({
        let (weak, c) = (ui.as_weak(), c.clone());
        move || {
            let ui = weak.unwrap();
            env_op(&ui, &c, |ws, i| ws.delete_env(i));
            load_env_text(&ui, &c.app.borrow().ws);
        }
    });
    let code_editing = ui.global::<CodeEditing>();
    // The software renderer treats "monospace" as a literal name; resolve the system's generic family first.
    let mut fonts = slint::fontique_011::shared_collection();
    let mono = fonts
        .generic_families(slint::fontique_011::fontique::GenericFamily::Monospace)
        .next();
    if let Some(family) = mono {
        code_editing.set_monospace_family(fonts.family_name(family).unwrap_or("DejaVu Sans Mono").into());
    }
    code_editing.on_indent(|text, anchor, caret, unindent| {
        editor::indent(&text, anchor.max(0) as usize, caret.max(0) as usize, unindent)
    });
    code_editing.on_enter(|text, anchor, caret, kind| {
        editor::enter(&text, anchor.max(0) as usize, caret.max(0) as usize, kind)
    });
    code_editing.on_format(|text, kind| payload::format(&text, kind).into());
    code_editing.on_line_numbers(|text| editor::line_numbers(&text).into());
    code_editing.on_selection_safe({
        let weak = ui.as_weak();
        // Selection offsets also invalidate the binding after a move to a different monitor.
        move |width, _, _| {
            weak.upgrade()
                .is_some_and(|ui| width * ui.window().scale_factor() < 32_000.0)
        }
    });
    let validation_timer = slint::Timer::default();
    ui.on_validate_body({
        let weak = ui.as_weak();
        move || {
            let Some(ui) = weak.upgrade() else { return };
            ui.set_body_validation(Validation::default());
            let weak = weak.clone();
            validation_timer.start(slint::TimerMode::SingleShot, Duration::from_millis(250), move || {
                if let Some(ui) = weak.upgrade() {
                    ui.set_body_validation(validation::check(&ui.get_body_text(), ui.get_body_type()));
                }
            });
        }
    });
    let var_assist = ui.global::<VarAssist>();
    var_assist.on_matches(|names, text, caret, all| {
        let names: Vec<String> = names.iter().map(String::from).collect();
        list(editor::var_matches(&names, &text, caret.max(0) as usize, all))
    });
    var_assist.on_insert(|text, caret, name| editor::var_insert(&text, caret.max(0) as usize, &name).0.into());
    var_assist.on_insert_caret(|text, caret, name| editor::var_insert(&text, caret.max(0) as usize, &name).1 as i32);
    var_assist.on_caret(|old, new| editor::caret_after_edit(&old, &new) as i32);
    ui.on_transfer_prepare({
        let (weak, c) = (ui.as_weak(), c.clone());
        move |mode| transfer::prepare(&weak.unwrap(), &c, mode)
    });
    ui.on_export_toggled({
        let (weak, c) = (ui.as_weak(), c.clone());
        move |index, on| transfer::toggled(&weak.unwrap(), &c, index, on)
    });
    ui.on_transfer_browse({
        let weak = ui.as_weak();
        move || transfer::browse(&weak.unwrap())
    });
    ui.on_transfer_run({
        let (weak, c) = (ui.as_weak(), c.clone());
        move || transfer::run(&weak.unwrap(), &c)
    });
    ui.on_kv_edited({
        let tables = c.tables.clone();
        move |table, index, row| {
            let t = &tables[table as usize];
            if (index as usize) < t.row_count() {
                t.set_row_data(index as usize, row);
                ensure_blank(t);
            }
        }
    });
    ui.on_kv_removed({
        let tables = c.tables.clone();
        move |table, index| {
            let t = &tables[table as usize];
            if (index as usize) < t.row_count() {
                t.remove(index as usize);
                ensure_blank(t);
            }
        }
    });
    ui.on_send({
        let (weak, c) = (ui.as_weak(), c.clone());
        move || {
            let ui = weak.unwrap();
            if ui.get_sending() {
                return;
            }
            flush(&ui, &c);
            // The thread gets its own copy: it may send other requests first, for `{{ $response(...) }}`.
            let job = c.app.borrow().ws.request_parts().map(|(project, folders, request)| {
                (
                    project.clone(),
                    folders.into_iter().cloned().collect::<Vec<Folder>>(),
                    request.clone(),
                )
            });
            let Some((project, folders, request)) = job else { return };
            ui.set_error_kind(-1);
            highlight::clear_response(&ui);
            ui.set_request_log("".into());
            ui.set_warning_vars("".into());
            ui.set_sending(true);
            let (weak, generation, responses, dialogs) = (
                weak.clone(),
                c.generation.clone(),
                c.responses.clone(),
                c.dialogs.clone(),
            );
            let started_on = generation.load(Ordering::SeqCst);
            // Blocking work stays off the UI thread; the result comes back through the event loop.
            std::thread::spawn(move || {
                let responses = responses.for_send(generation.clone(), started_on);
                let result = chain::prepare_interactive(
                    &project,
                    &folders.iter().collect::<Vec<_>>(),
                    &request,
                    &responses,
                    &mut |spec| dialogs.ask(&weak, &generation, started_on, spec.clone(), false),
                    &mut |prepared| {
                        if generation.load(Ordering::SeqCst) != started_on {
                            return Err(Error::Cancelled);
                        }
                        if let http::Payload::File(path) = &prepared.payload {
                            dialogs.ask(
                                &weak,
                                &generation,
                                started_on,
                                fastapiclient_core::resolve::Prompt {
                                    label: path.display().to_string(),
                                    description: prepared.host(),
                                    default: String::new(),
                                },
                                true,
                            )?;
                        }
                        if generation.load(Ordering::SeqCst) != started_on {
                            return Err(Error::Cancelled);
                        }
                        Ok(())
                    },
                )
                .and_then(|prepared| {
                    // Shown now, so the request is visible while it waits for the response.
                    let (log, missing) = (http::request_log(&prepared), prepared.missing.join(", "));
                    let (ui, generation) = (weak.clone(), generation.clone());
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = ui.upgrade().filter(|_| generation.load(Ordering::SeqCst) == started_on) {
                            ui.set_warning_vars(missing.into());
                            ui.set_request_log(log.into());
                        }
                    });
                    responses.invalidate(&project, &request.name)?;
                    http::send(prepared)
                })
                .inspect(|r| {
                    // Names can repeat: only the request `$response` would fetch may own the cached value.
                    if project.owns_response_name(&request) {
                        responses.store(&project, &request.name, r);
                    }
                });
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = weak.upgrade() else { return };
                    if generation.load(Ordering::SeqCst) != started_on {
                        return; // another request was selected meanwhile
                    }
                    ui.set_sending(false);
                    match result {
                        Ok(response) => show_response(&ui, response),
                        Err(e) => show_error(&ui, e),
                    }
                });
            });
        }
    });
    ui.on_send_prompt_reply({
        let c = c.clone();
        move |accepted, text| {
            c.dialogs
                .reply(c.generation.load(Ordering::SeqCst), accepted.then(|| text.to_string()))
        }
    });
    ui.on_send_confirmed({
        let c = c.clone();
        move || {
            c.dialogs
                .reply(c.generation.load(Ordering::SeqCst), Some(String::new()))
        }
    });
    ui.on_find(|hay, needle, from, backward| {
        fastapiclient_core::find::find(&hay, &needle, from.max(0) as usize, backward)
            .map(|f| Found {
                start: f.start as i32,
                end: f.end as i32,
                index: f.index as i32,
                total: f.total as i32,
            })
            .unwrap_or_default()
    });
    ui.on_open_external(|text| {
        if let Err(e) = open_external(&text) {
            eprintln!("cannot open the response: {e}");
        }
    });
    ui.on_open_website(|| {
        // Waiting in a thread reaps the child; a dropped `Child` would stay a zombie until exit.
        std::thread::spawn(|| {
            if let Err(e) = std::process::Command::new("xdg-open")
                .arg(env!("CARGO_PKG_REPOSITORY"))
                .status()
            {
                eprintln!("cannot open the website: {e}");
            }
        });
    });
    let settings = Rc::new(RefCell::new(settings));
    // Saved as soon as it changes: a killed session would otherwise lose the choice.
    ui.on_theme_changed({
        let (c, settings) = (c.clone(), settings.clone());
        move |i| {
            let theme = THEMES.get(i as usize).copied().unwrap_or_default();
            let mut s = settings.borrow_mut();
            // Also fires once at start-up; saving then would rewrite the file for nothing.
            if s.theme != theme && c.app.borrow().persist {
                s.theme = theme.into();
                s.save();
            }
        }
    });
    // The language applies at once and is saved like the theme.
    ui.on_language_changed({
        let (weak, c, settings) = (ui.as_weak(), c.clone(), settings.clone());
        move |i| {
            let code = LANGUAGES.get(i as usize).copied().unwrap_or("");
            {
                let mut s = settings.borrow_mut();
                if s.language != code && c.app.borrow().persist {
                    s.language = code.into();
                    s.save();
                }
            }
            let chosen = Settings {
                language: code.into(),
                ..Settings::default()
            };
            // An unbundled system locale returns Err and would keep the old language; fall back to the source strings.
            let _ = slint::select_bundled_translation(&language(&chosen))
                .or_else(|_| slint::select_bundled_translation(""));
            // Text built in Rust (the environment list) is still in the old language until it is built again.
            if let Some(ui) = weak.upgrade() {
                refresh(&ui, &c, false);
            }
        }
    });
    // Window size and selection are remembered for the next start.
    ui.window().on_close_requested({
        let (weak, c, settings) = (ui.as_weak(), c.clone(), settings.clone());
        move || {
            c.generation.fetch_add(1, Ordering::SeqCst);
            c.dialogs.cancel();
            if let Some(ui) = weak.upgrade() {
                ui.set_sending(false);
                if matches!(ui.get_prompt_kind(), 8 | 9) {
                    ui.set_prompt_open(false);
                }
                flush(&ui, &c);
                let app = c.app.borrow();
                if !app.dirty.is_empty() {
                    return slint::CloseRequestResponse::KeepWindowShown;
                }
                if app.persist {
                    let mut settings = settings.borrow_mut();
                    let size = ui.window().size().to_logical(ui.window().scale_factor());
                    (settings.width, settings.height) = (size.width.round() as u32, size.height.round() as u32);
                    let sel = app.ws.sel;
                    settings.last_project = sel
                        .and_then(|s| app.ws.projects.get(s.project))
                        .map(|p| p.id.clone())
                        .unwrap_or_default();
                    (settings.last_folder, settings.last_request) =
                        (sel.and_then(|s| s.folder), sel.and_then(|s| s.request));
                    settings.auto_beautify = ui.get_auto_beautify();
                    settings.sidebar_width = ui.get_sidebar_width();
                    settings.request_split = ui.get_request_split();
                    settings.theme = THEMES
                        .get(ui.get_color_scheme_index() as usize)
                        .copied()
                        .unwrap_or_default()
                        .into();
                    settings.language = LANGUAGES
                        .get(ui.get_language_index() as usize)
                        .copied()
                        .unwrap_or_default()
                        .into();
                    settings.save();
                }
            }
            slint::CloseRequestResponse::HideWindow
        }
    });
    let (width, height) = (settings.borrow().width, settings.borrow().height);
    ui.window()
        .set_size(slint::LogicalSize::new(width as f32, height as f32));
}

fn show_response(ui: &Main, r: Response) {
    ui.set_status(r.status.into());
    ui.set_reason(r.reason.into());
    ui.set_time_ms(r.elapsed.as_millis().min(i32::MAX as u128) as i32);
    ui.set_size_bytes(r.body.len() as i32);
    ui.set_truncated(r.truncated);
    let body = String::from_utf8_lossy(&r.body);
    let content_type = r
        .headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("content-type"))
        .map(|(_, value)| value.as_str())
        .unwrap_or("");
    ui.set_has_response(true);
    highlight::response(ui, &body, content_type);
    let headers: Vec<String> = r.headers.iter().map(|(k, v)| format!("{k}: {v}")).collect();
    ui.set_response_headers(headers.join("\n").into());
    ui.set_has_response(true);
}

/// Only the kind and the raw detail go to the UI; the wording is `Tr.error` in main.slint.
fn show_error(ui: &Main, e: Error) {
    // `extra` is the name of the request a `$response` needed.
    let (kind, detail, extra) = match e {
        Error::Cancelled => return,
        Error::Url => (0, String::new(), String::new()),
        Error::Method => (1, String::new(), String::new()),
        Error::Header(h) => (2, h, String::new()),
        Error::BodyFile(d) => (3, d, String::new()),
        Error::OAuthPending => (4, String::new(), String::new()),
        Error::Network(d) => (5, d, String::new()),
        Error::Dependency(name, d) => (6, d, name),
        Error::DependencyJson(name) => (8, String::new(), name),
        Error::CacheLimit => (9, String::new(), String::new()),
        Error::Cycle(name) => (7, String::new(), name),
    };
    ui.set_error_kind(kind);
    ui.set_error_detail(detail.into());
    ui.set_error_extra(extra.into());
}

/// `settings.theme` for each entry of the Appearance list: system, light, dark.
const THEMES: [&str; 3] = ["", "light", "dark"];

/// The Appearance list index of a `settings.theme` value; anything unknown follows the system.
fn theme_index(theme: &str) -> i32 {
    THEMES.iter().position(|t| *t == theme).unwrap_or(0) as i32
}

/// `settings.language` for each entry of the Language list: the system's, English, Português.
const LANGUAGES: [&str; 3] = ["", "en", "pt_BR"];

/// `settings.language`, or the system locale (`LC_ALL`/`LC_MESSAGES`/`LANGUAGE`/`LANG`, minus `.UTF-8`).
fn language(settings: &Settings) -> String {
    if !settings.language.is_empty() {
        return settings.language.clone();
    }
    ["LC_ALL", "LC_MESSAGES", "LANGUAGE", "LANG"]
        .iter()
        .filter_map(|v| std::env::var(v).ok())
        .find(|v| !v.is_empty())
        .map(|v| v.split(['.', ':']).next().unwrap_or("").to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{THEMES, guess_ext, theme_index};

    #[test]
    fn themes_and_their_indexes_agree() {
        for (i, t) in THEMES.iter().enumerate() {
            assert_eq!(theme_index(t), i as i32);
        }
        assert_eq!(theme_index("bogus"), 0);
    }

    #[test]
    fn guesses_extension() {
        assert_eq!(guess_ext(" {\"a\":1}"), "json");
        assert_eq!(guess_ext("[1]"), "json");
        // A server's HTML must not open in a browser, where its scripts would run.
        assert_eq!(guess_ext("<!DOCTYPE html><p>"), "txt");
        assert_eq!(guess_ext("<?xml version=\"1.0\"?>"), "xml");
        assert_eq!(guess_ext("hello"), "txt");
    }
}
