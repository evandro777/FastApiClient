//! The worker waits on a reply channel; selecting another request drops it immediately.
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
};

use fastapiclient_core::{http::Error, resolve::Prompt};

use crate::Main;

#[derive(Default)]
pub struct Dialogs {
    pending: Mutex<Option<(u64, String, mpsc::Sender<String>)>>,
    last_answers: Mutex<HashMap<String, String>>,
}

impl Dialogs {
    pub fn cancel(&self) {
        self.pending.lock().unwrap().take();
    }

    pub fn reply(&self, generation: u64, answer: Option<String>) {
        if let Some((started, label, tx)) = self.pending.lock().unwrap().take()
            && started == generation
            && let Some(answer) = answer
        {
            self.last_answers.lock().unwrap().insert(label, answer.clone());
            let _ = tx.send(answer);
        }
    }

    fn remembered_or_default(&self, spec: &Prompt) -> String {
        self.last_answers
            .lock()
            .unwrap()
            .get(&spec.label)
            .cloned()
            .unwrap_or_else(|| spec.default.clone())
    }

    pub fn ask(
        self: &Arc<Self>,
        weak: &slint::Weak<Main>,
        generation: &Arc<AtomicU64>,
        started: u64,
        spec: Prompt,
        file: bool,
    ) -> Result<String, Error> {
        let (tx, rx) = mpsc::channel();
        let default = self.remembered_or_default(&spec);
        let label = spec.label.clone();
        let (dialogs, generation, weak) = (self.clone(), generation.clone(), weak.clone());
        slint::invoke_from_event_loop(move || {
            if generation.load(Ordering::SeqCst) != started {
                return;
            }
            let Some(ui) = weak.upgrade() else { return };
            *dialogs.pending.lock().unwrap() = Some((started, label, tx));
            ui.set_send_phase(if file { 3 } else { 2 });
            ui.set_send_method("".into());
            ui.set_send_host("".into());
            if file {
                ui.set_send_file_path(spec.label.into());
                ui.set_send_file_host(spec.description.into());
            } else {
                ui.set_send_prompt_title(spec.label.into());
                ui.set_send_prompt_description(spec.description.into());
            }
            ui.set_prompt_text(default.into());
            ui.set_prompt_kind(if file { 8 } else { 9 });
            ui.set_prompt_open(true);
        })
        .map_err(|_| Error::Cancelled)?;
        rx.recv().map_err(|_| Error::Cancelled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_answers_cancel_and_stale_replies_release_the_waiter() {
        let dialogs = Dialogs::default();
        for (generation, answer, expected) in [
            (1, Some(String::new()), Some(String::new())),
            (1, None, None),
            (2, Some("stale".into()), None),
        ] {
            let (tx, rx) = mpsc::channel();
            *dialogs.pending.lock().unwrap() = Some((1, "label".into(), tx));
            dialogs.reply(generation, answer);
            assert_eq!(rx.recv().ok(), expected);
        }
        let (tx, rx) = mpsc::channel();
        *dialogs.pending.lock().unwrap() = Some((1, "label".into(), tx));
        dialogs.cancel();
        assert!(rx.recv().is_err());
        dialogs.reply(1, Some("too late".into()));
    }

    #[test]
    fn accepted_answers_are_remembered_per_label_for_the_session() {
        let dialogs = Dialogs::default();
        let spec = Prompt {
            label: "Process ID".into(),
            description: String::new(),
            default: "default-id".into(),
        };
        assert_eq!(dialogs.remembered_or_default(&spec), "default-id");

        let (tx, rx) = mpsc::channel();
        *dialogs.pending.lock().unwrap() = Some((1, spec.label.clone(), tx));
        dialogs.reply(1, Some("12345".into()));
        assert_eq!(rx.recv().unwrap(), "12345");
        assert_eq!(dialogs.remembered_or_default(&spec), "12345");

        let other = Prompt {
            label: "Account ID".into(),
            ..spec.clone()
        };
        assert_eq!(dialogs.remembered_or_default(&other), "default-id");
    }
}
