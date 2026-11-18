use super::*;
use std::sync::mpsc::Receiver;

// Slint has one process-wide event-loop proxy: share the response test's deterministic queue.
pub fn send_and_filter(events: &Receiver<Box<dyn FnOnce() + Send>>) {
    let ui = Main::new().unwrap();
    let c = Ctx {
        app: Rc::new(RefCell::new(App {
            ws: fixtures::workspace(2),
            persist: false,
            dirty: HashSet::new(),
        })),
        tables: std::array::from_fn(|_| Rc::new(VecModel::default())),
        methods: Rc::new(VecModel::default()),
        tree: Rc::new(VecModel::default()),
        export_rows: Rc::new(VecModel::default()),
        generation: Arc::default(),
        responses: Arc::default(),
        dialogs: Arc::default(),
    };
    ui.set_tree_rows(c.tree.clone().into());
    wire(&ui, &c, Settings::default());
    refresh(&ui, &c, true);
    ui.set_body_text("unsaved body".into());
    let selected = c.app.borrow().ws.sel;
    let generation = c.generation.load(Ordering::SeqCst);
    ui.set_request_filter("no match".into());
    ui.invoke_filter_edited();
    assert_eq!(c.tree.row_count(), 0);
    assert_eq!(c.app.borrow().ws.sel, selected);
    assert_eq!(ui.get_body_text(), "unsaved body");
    assert_eq!(c.generation.load(Ordering::SeqCst), generation);
    ui.set_request_filter("request 3".into());
    ui.invoke_filter_edited();
    assert_eq!(c.tree.row_count(), 2);
    ui.invoke_step(1); // hidden selection -> first visible row (folder)
    ui.invoke_step(1); // visible request
    assert_eq!(ui.get_selected_name(), "Request 3");
    ui.set_request_filter("".into());
    ui.invoke_filter_edited();
    assert!(c.tree.row_count() > 2);
    ui.set_request_filter("request".into());
    ui.invoke_project_activated(1);
    assert!(ui.get_request_filter().is_empty());
    assert_eq!(c.tree.row_count(), 1);

    creation(&ui, &c);
    save_recovery(&ui, &c);
    deletion_failures(&ui, &c);
    failed_manual_refresh(&ui, &c, events);
    environment_changes(&ui, &c, events);

    let mut project = Project::new("Prompts");
    project.requests = vec![
        fastapiclient_core::model::Request {
            name: "Upload".into(),
            url: "{{ $prompt('Host', '', 'http://localhost:9') }}/upload".into(),
            body: Body::File("{{ $prompt('Path') }}".into()),
            ..Default::default()
        },
        fastapiclient_core::model::Request::default(),
    ];
    c.app.borrow_mut().ws = Workspace::new(vec![project]);
    c.app.borrow_mut().ws.select(Sel {
        project: 0,
        folder: None,
        request: Some(0),
    });
    refresh(&ui, &c, true);
    assert!(!ui.get_prompt_open(), "editing must never prompt");
    let pump = |done: &dyn Fn() -> bool| {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !done() {
            events
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .expect("Send worker event")();
        }
    };
    ui.invoke_send();
    pump(&|| ui.get_prompt_open());
    assert_eq!(ui.get_prompt_kind(), 9);
    assert_eq!(ui.get_send_prompt_title(), "Host");
    assert_eq!(ui.get_prompt_text(), "http://localhost:9");
    ui.set_prompt_open(false);
    ui.invoke_send_prompt_reply(true, "http://localhost:9".into());
    pump(&|| ui.get_prompt_open());
    assert_eq!(ui.get_send_prompt_title(), "Path");
    ui.set_prompt_open(false);
    ui.invoke_send_prompt_reply(true, "/tmp/example-body".into());
    pump(&|| ui.get_prompt_open());
    assert_eq!(ui.get_prompt_kind(), 8);
    assert_eq!(ui.get_send_file_path(), "/tmp/example-body");
    assert_eq!(ui.get_send_file_host(), "localhost:9");
    ui.set_prompt_open(false);
    ui.invoke_send_prompt_reply(false, "".into());
    pump(&|| !ui.get_sending());
    assert_eq!(ui.get_error_kind(), -1);

    ui.invoke_send();
    pump(&|| ui.get_prompt_open());
    assert_eq!(ui.get_send_prompt_title(), "Host", "new Send must forget answers");
    ui.invoke_select(0, -1, 1);
    assert!(!ui.get_prompt_open());
    assert!(!ui.get_sending());
    ui.invoke_send_prompt_reply(true, "stale".into());
    events
        .recv_timeout(Duration::from_secs(10))
        .expect("cancelled completion")();
    assert_eq!(ui.get_error_kind(), -1);

    // A prompt publication already queued when selection changes must never open its dialog.
    ui.invoke_select(0, -1, 0);
    ui.invoke_send();
    let queued = events.recv_timeout(Duration::from_secs(10)).expect("queued prompt");
    ui.invoke_select(0, -1, 1);
    queued();
    events.recv_timeout(Duration::from_secs(10)).expect("stale completion")();
    assert!(!ui.get_prompt_open());
    assert_eq!(ui.get_error_kind(), -1);

    ui.invoke_select(0, -1, 0);
    ui.invoke_send();
    pump(&|| ui.get_prompt_open());
    let generation = c.generation.load(Ordering::SeqCst);
    ui.window().dispatch_event(slint::platform::WindowEvent::CloseRequested);
    assert!(c.generation.load(Ordering::SeqCst) > generation);
    events
        .recv_timeout(Duration::from_secs(10))
        .expect("window-close cancellation")();
}

fn environment_changes(ui: &Main, c: &Ctx, events: &Receiver<Box<dyn FnOnce() + Send>>) {
    use fastapiclient_core::model::{Environment, Request};
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::mpsc,
    };

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut project = Project::new("Environments");
    project.environments = ["Development", "Production"]
        .map(|name| Environment {
            name: name.into(),
            values: serde_json::json!({"host":"unused"}),
        })
        .into();
    project.active_env = "Development".into();
    project.requests.push(Request {
        name: "Read".into(),
        url: format!("http://{}", listener.local_addr().unwrap()),
        ..Request::default()
    });
    c.app.borrow_mut().ws = Workspace::new(vec![project]);
    c.app.borrow_mut().ws.select(Sel {
        project: 0,
        folder: None,
        request: Some(0),
    });
    refresh(ui, c, true);
    let (accepted, ready) = mpsc::channel();
    let (release, resume) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        stream.read_exact(&mut [0; 1]).unwrap();
        accepted.send(()).unwrap();
        resume.recv_timeout(Duration::from_secs(10)).unwrap();
        let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}");
    });
    ui.invoke_send();
    ready.recv_timeout(Duration::from_secs(10)).unwrap();
    events.recv_timeout(Duration::from_secs(10)).expect("request log")();
    ui.set_url("unsaved request draft".into());
    let started = c.generation.load(Ordering::SeqCst);
    ui.invoke_env_activated(2);
    assert!(c.generation.load(Ordering::SeqCst) > started);
    assert!(!ui.get_sending());
    assert!(ui.get_request_log().is_empty());
    assert_eq!(ui.get_url(), "unsaved request draft");
    release.send(()).unwrap();
    worker.join().unwrap();
    events
        .recv_timeout(Duration::from_secs(10))
        .expect("cancelled response")();
    assert!(!ui.get_has_response(), "old environment's response was published");

    ui.set_has_response(true);
    let started = c.generation.load(Ordering::SeqCst);
    ui.set_env_edit_index(1);
    ui.invoke_env_save(r#"{"host":"changed"}"#.into());
    assert!(c.generation.load(Ordering::SeqCst) > started);
    assert!(!ui.get_has_response());
    assert_eq!(ui.get_url(), "unsaved request draft");
    let started = c.generation.load(Ordering::SeqCst);
    ui.invoke_env_activated(2);
    assert_eq!(
        c.generation.load(Ordering::SeqCst),
        started,
        "unchanged environment cancelled Send"
    );
}

fn failed_manual_refresh(ui: &Main, c: &Ctx, events: &Receiver<Box<dyn FnOnce() + Send>>) {
    use fastapiclient_core::chain::Lookup;
    use std::{io::Read, net::TcpListener};

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut project = Project::new("Refresh");
    project.requests.push(fastapiclient_core::model::Request {
        name: "Token".into(),
        url: format!("http://{}", listener.local_addr().unwrap()),
        ..Default::default()
    });
    let response = Response {
        status: 200,
        reason: "OK".into(),
        headers: vec![],
        body: br#"{"access_token":"old"}"#.to_vec(),
        truncated: false,
        elapsed: Duration::ZERO,
    };
    c.responses.store(&project, "Token", &response);
    c.responses.store(&project, "Other", &response);
    c.app.borrow_mut().ws = Workspace::new(vec![project.clone()]);
    c.app.borrow_mut().ws.select(Sel {
        project: 0,
        folder: None,
        request: Some(0),
    });
    refresh(ui, c, true);
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let _ = socket.read(&mut [0; 1]);
        // Close without an HTTP response to exercise a network failure, rather than a non-2xx answer.
    });
    ui.invoke_send();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while ui.get_sending() {
        events
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            .expect("manual refresh completion")();
    }
    server.join().unwrap();
    assert!(!ui.get_has_response());
    assert_ne!(ui.get_error_kind(), -1);
    assert!(matches!(
        c.responses
            .lookup(&project, "$response('Token', '$.access_token')", true),
        Lookup::Stale(_)
    ));
    assert!(matches!(
        c.responses.lookup(&project, "$response('Other', '$.access_token')", true),
        Lookup::Value(value) if value == "old"
    ));
}

fn creation(ui: &Main, c: &Ctx) {
    let default_name = |ui: &Main, kind: i32| ui.global::<Tr>().invoke_default_name(kind);
    c.app.borrow_mut().ws = fixtures::workspace(1);
    refresh(ui, c, true);
    ui.set_body_text("unsaved body".into());
    let before = c.app.borrow().ws.projects.clone();
    let selected = c.app.borrow().ws.sel;
    for folder in [false, true] {
        ui.invoke_ask_create(folder);
        assert!(ui.get_prompt_open());
        assert_eq!(ui.get_prompt_kind(), if folder { 10 } else { 11 });
        assert_eq!(ui.get_prompt_text(), default_name(ui, if folder { 1 } else { 2 }));
        assert_eq!(c.app.borrow().ws.projects, before);
        assert_eq!(c.app.borrow().ws.sel, selected);
        assert_eq!(ui.get_body_text(), "unsaved body");
        ui.set_prompt_open(false);
        if folder {
            ui.invoke_new_folder(" \t\n ".into());
        } else {
            ui.invoke_new_request(" \t\n ".into());
        }
        assert_eq!(c.app.borrow().ws.projects, before);
        assert_eq!(c.app.borrow().ws.sel, selected);
        assert_eq!(ui.get_body_text(), "unsaved body");
    }
    ui.invoke_new_request(default_name(ui, 2));
    assert_eq!(c.app.borrow().ws.projects[0].requests.len(), 2);
    assert_eq!(
        c.app.borrow().ws.projects[0].requests[0].body,
        Body::Json("unsaved body".into())
    );
    assert_eq!(ui.get_selected_name(), default_name(ui, 2));
    ui.invoke_select(0, -1, -1);
    ui.invoke_new_folder(default_name(ui, 1));
    assert_eq!(c.app.borrow().ws.projects[0].folders.len(), 1);
    assert_eq!(c.app.borrow().ws.projects[0].folders[0].parent, None);
    ui.invoke_new_folder("  Nested  ".into());
    assert_eq!(c.app.borrow().ws.projects[0].folders.len(), 2);
    assert_eq!(c.app.borrow().ws.projects[0].folders[1].parent, Some(0));
    assert_eq!(ui.get_selected_name(), "Nested");
    for folder in [0, 1] {
        ui.invoke_select(0, folder, -1);
        ui.invoke_new_request("  Custom  ".into());
        let app = c.app.borrow();
        let requests = &app.ws.projects[0].folders[folder as usize].requests;
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].name, "Custom");
        assert_eq!(app.ws.sel.unwrap().folder, Some(folder as usize));
    }
    ui.invoke_rename("Renamed".into());
    assert_eq!(ui.get_selected_name(), "Renamed");
    ui.invoke_delete_selected();
    assert!(c.app.borrow().ws.projects[0].folders[1].requests.is_empty());
    ui.invoke_select(0, -1, -1);
    ui.invoke_new_request("  Root  ".into());
    assert_eq!(c.app.borrow().ws.projects[0].requests.len(), 3);
    assert_eq!(ui.get_selected_name(), "Root");
}

fn save_recovery(ui: &Main, c: &Ctx) {
    let mut app = App {
        ws: fixtures::workspace(2),
        persist: true,
        dirty: HashSet::new(),
    };
    // Pending projects may no longer be selected when the timer retries them.
    app.dirty.extend(app.ws.projects.iter().map(|p| p.id.clone()));
    let first = app.ws.projects[0].id.clone();
    assert!(!retry_saves(ui, &mut app, |project| {
        if project.id == first {
            Err(std::io::Error::other("disk full"))
        } else {
            Ok(())
        }
    }));
    assert_eq!(app.dirty.len(), 1);
    assert_eq!(ui.get_save_error(), "disk full");
    let mut retried = 0;
    assert!(retry_saves(ui, &mut app, |project| {
        assert_eq!(project.id, first);
        retried += 1;
        Ok(())
    }));
    assert_eq!(retried, 1);
    assert!(ui.get_save_error().is_empty());
    app.dirty.insert("deleted-project".into());
    assert!(retry_saves(ui, &mut app, |_| panic!(
        "a removed project must not be written"
    )));

    c.app.borrow_mut().ws = fixtures::workspace(1);
    c.app.borrow_mut().ws.projects[0].id = "../invalid".into();
    refresh(ui, c, true);
    c.app.borrow_mut().persist = true;
    ui.set_url("http://edited".into());
    flush(ui, c); // invalid id injects a real store failure without writing a file
    assert_eq!(c.app.borrow().dirty.len(), 1);
    assert!(!ui.get_save_error().is_empty());
    flush(ui, c); // unchanged editor must still retry
    assert_eq!(c.app.borrow().dirty.len(), 1);
    ui.show().unwrap();
    for kind in [8, 9] {
        ui.set_sending(true);
        ui.set_prompt_kind(kind);
        ui.set_prompt_open(true);
        ui.window().dispatch_event(slint::platform::WindowEvent::CloseRequested);
        assert!(ui.window().is_visible(), "failed saves must prevent silent close");
        assert!(!ui.get_sending(), "a refused close must leave Send usable");
        assert!(!ui.get_prompt_open(), "cancelled send dialogs must close too");
    }
    assert!(retry_saves(ui, &mut c.app.borrow_mut(), |_| Ok(())));
    c.app.borrow_mut().persist = false;
    ui.hide().unwrap();
}

fn deletion_failures(ui: &Main, c: &Ctx) {
    c.app.borrow_mut().ws = fixtures::workspace(1);
    c.app.borrow_mut().ws.projects[0].id = "../invalid".into();
    c.app.borrow_mut().persist = true;
    c.app.borrow_mut().dirty.insert("../invalid".into());
    refresh(ui, c, true);
    let project = c.app.borrow().ws.projects[0].clone();
    let cached = Response {
        status: 200,
        reason: "OK".into(),
        headers: vec![],
        body: br#"{"token":"old"}"#.to_vec(),
        truncated: false,
        elapsed: Duration::ZERO,
    };
    assert!(c.responses.store(&project, "Token", &cached));
    ui.set_url("http://unsaved-draft".into());
    let selected = c.app.borrow().ws.sel;
    ui.invoke_delete_project();
    assert!(!ui.get_delete_error().is_empty());
    assert_eq!(c.app.borrow().ws.projects.len(), 1);
    assert_eq!(c.app.borrow().ws.sel, selected);
    assert_eq!(ui.get_url(), "http://unsaved-draft");
    assert!(c.app.borrow().dirty.contains("../invalid"));
    assert!(matches!(
        c.responses.lookup(&project, "$response('Token', '$.token')", true),
        fastapiclient_core::chain::Lookup::Value(_)
    ));

    c.app.borrow_mut().ws.select_project(0);
    refresh(ui, c, true);
    ui.invoke_delete_selected();
    assert!(!ui.get_delete_error().is_empty());
    assert_eq!(c.app.borrow().ws.projects.len(), 1);
    c.app.borrow_mut().persist = false;
    let old = c
        .responses
        .for_send(c.generation.clone(), c.generation.load(Ordering::SeqCst));
    ui.invoke_delete_selected();
    assert!(ui.get_delete_error().is_empty());
    assert!(c.app.borrow().ws.projects.is_empty());
    assert!(c.app.borrow().dirty.is_empty());
    assert!(!old.store(&project, "Token", &cached));
    assert!(matches!(
        c.responses.lookup(&project, "$response('Token', '$.token')", true),
        fastapiclient_core::chain::Lookup::Stale(_)
    ));
}
