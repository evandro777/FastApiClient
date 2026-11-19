use super::*;
use slint::platform::{EventLoopProxy, Platform, WindowAdapter, software_renderer::MinimalSoftwareWindow};
use std::rc::Rc;

#[test]
fn response_limits_invalid_documents_and_manual_colors() {
    for source in ["<name> Ana </name>", "<n> </n>", "<n>\n </n>"] {
        assert_eq!(response_display(source, 4, false), (source.into(), 0));
    }
    for source in [
        "<p>Hello <b>world</b>!</p>",
        "<n xml:space='preserve'><a/></n>",
        "<n><![CDATA[ ]]><a/></n>",
        "<n>&#32;<a/></n>",
    ] {
        assert_eq!(response_display(source, 4, false), (source.into(), 1));
    }
    for size in [AUTO_TEXT_LIMIT - 1, AUTO_TEXT_LIMIT, AUTO_TEXT_LIMIT + 1] {
        let source = format!("[0]{}", " ".repeat(size - 3));
        assert_eq!(
            response_display(&source, 3, true).1,
            if size <= AUTO_TEXT_LIMIT { 0 } else { 2 }
        );
        assert_eq!(response_display(&source, 3, false), ("[\n  0\n]".into(), 0));
        // The formatted output has four more bytes than this compact input.
        let source = format!("[\"{}\"]", "x".repeat(size - 8));
        let (display, notice) = response_display(&source, 3, true);
        assert_eq!(notice, if size <= AUTO_TEXT_LIMIT { 0 } else { 2 });
        if notice == 0 {
            assert_eq!(display.len(), size);
        }
    }
    for (source, kind) in [
        (r#"{"n":01}"#, 3),
        ("{", 3),
        ("<a><b></a>", 4),
        ("<a/><b/>", 4),
        ("<a x=no/>", 4),
    ] {
        assert_eq!(response_display(source, kind, false), (source.into(), 1));
    }
    assert_eq!(response_display(r#""\u00e3""#, 3, false), ("\"ã\"".into(), 0));
    let source = format!("[\"{}\"]", "x".repeat(MAX_BODY - 4));
    let (display, notice) = response_display(&source, 3, false);
    assert_eq!(notice, 0);
    assert!(display.len() > MAX_BODY);
    let syntaxes = SyntaxSet::load_defaults_newlines();
    let lines = colors(&display, 3, MANUAL_OUTPUT_LIMIT, &syntaxes, &theme()).unwrap();
    assert!(lines.iter().flatten().any(|run| run.kind == 1));
    assert!(colors(&display, 3, AUTO_TEXT_LIMIT, &syntaxes, &theme()).is_none());
    let xml = format!("{}<leaf/>{}", "<r>".repeat(1500), "</r>".repeat(1500));
    assert_eq!(response_display(&xml, 4, false), (String::new(), 2));
    let depth = 1445;
    let overhead = 2 * depth * depth + 9 * depth + 13;
    for size in [MANUAL_OUTPUT_LIMIT - 1, MANUAL_OUTPUT_LIMIT, MANUAL_OUTPUT_LIMIT + 1] {
        let xml = format!(
            "{}<leaf>{}</leaf>{}",
            "<r>".repeat(depth),
            "x".repeat(size - overhead),
            "</r>".repeat(depth)
        );
        let (display, notice) = response_display(&xml, 4, false);
        assert_eq!(notice, if size <= MANUAL_OUTPUT_LIMIT { 0 } else { 2 });
        if notice == 0 {
            assert_eq!(display.len(), size);
        }
    }
}

// Drive the real UI callbacks and worker without X11; queued publications can be held to test races deterministically.
struct TestPlatform(mpsc::Sender<Box<dyn FnOnce() + Send>>);

impl Platform for TestPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(MinimalSoftwareWindow::new(Default::default()))
    }
    fn new_event_loop_proxy(&self) -> Option<Box<dyn EventLoopProxy>> {
        Some(Box::new(Self(self.0.clone())))
    }
}

impl EventLoopProxy for TestPlatform {
    fn quit_event_loop(&self) -> Result<(), slint::EventLoopError> {
        Ok(())
    }
    fn invoke_from_event_loop(&self, event: Box<dyn FnOnce() + Send>) -> Result<(), slint::EventLoopError> {
        self.0.send(event).unwrap();
        Ok(())
    }
}

#[test]
fn response_modes_cache_preference_and_stale_publications() {
    let (events, publications) = mpsc::channel();
    slint::platform::set_platform(Box::new(TestPlatform(events))).unwrap();
    let ui = Main::new().unwrap();
    wire(&ui);
    ui.set_has_response(true);
    let source = " \n{\"name\":\"n\\u00e3o\",\"n\":01}\t";
    ui.set_auto_beautify(false);
    response(&ui, source, "application/json");
    assert_eq!(ui.get_response_mode(), 0);
    assert_eq!(ui.get_response_body(), source);
    assert!(!ui.get_response_processing());
    assert!(ui.global::<CodeEditing>().get_response().source.is_empty());

    let publish = || {
        publications
            .recv_timeout(Duration::from_secs(15))
            .expect("worker publication")
    };
    let choose = |mode| {
        ui.set_response_mode(mode);
        ui.invoke_response_view(false);
    };
    choose(1);
    assert!(ui.get_response_processing());
    publish()();
    assert_eq!(ui.get_response_mode(), 1);
    assert_eq!(ui.get_response_pretty(), source);
    assert_eq!(ui.get_response_notice(), 1);
    assert_eq!(ui.global::<CodeEditing>().get_response().source, source);
    choose(0);
    choose(1);
    assert!(!ui.get_response_processing(), "cached toggle scheduled work");

    ui.set_auto_beautify(true);
    response(&ui, r#"{"new":1}"#, "application/json");
    let stale = publish();
    choose(0);
    stale();
    assert_eq!(ui.get_response_mode(), 0);
    assert!(!ui.get_response_ready());
    assert!(!ui.get_response_processing());

    choose(1);
    let stale = publish();
    response(&ui, "<latest/>", "application/xml");
    stale();
    assert!(!ui.get_response_ready());
    publish()();
    assert_eq!(ui.get_response_pretty(), "<latest/>");
    assert_eq!(ui.get_response_mode(), 1, "new response did not follow preference");

    response(&ui, "[123]", "application/json");
    let stale = publish();
    clear_response(&ui);
    stale();
    assert!(!ui.get_response_processing());
    assert!(!ui.get_response_ready(), "request change accepted a stale publication");
    ui.set_has_response(true);

    let large = format!("[0]{}", " ".repeat(AUTO_TEXT_LIMIT));
    response(&ui, &large, "application/json");
    assert_eq!(ui.get_response_mode(), 0);
    assert!(!ui.get_response_processing());
    choose(1);
    publish()();
    assert_eq!(ui.get_response_mode(), 1);
    ui.invoke_response_view(true);
    assert_eq!(
        ui.get_response_mode(),
        0,
        "preference ignored original size after a manual result"
    );
    choose(1);
    assert!(!ui.get_response_processing());
    ui.set_auto_beautify(false);
    ui.invoke_response_view(true);
    assert_eq!(ui.get_response_mode(), 0);
    response(&ui, "{}", "application/json");
    assert!(!ui.get_response_processing());

    for (source, mime) in [("<html/>", "text/html"), ("hello", "text/plain")] {
        response(&ui, source, mime);
        choose(1);
        assert!(!ui.get_response_processing());
        assert_eq!(ui.get_response_mode(), 0);
    }
    crate::app_tests::send_and_filter(&publications);
    crate::app_tests::request_switch_keeps_responses(&publications);
}
