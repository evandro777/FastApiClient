//! The worker waits on a reply channel; selecting another request drops it immediately.
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
    mpsc,
};

use fastapiclient_core::{http::Error, resolve::Prompt};

use crate::Main;

#[derive(Default)]
pub struct Dialogs {
    pending: Mutex<Option<(u64, mpsc::Sender<String>)>>,
}

impl Dialogs {
    pub fn cancel(&self) {
        self.pending.lock().unwrap().take();
    }

    pub fn reply(&self, generation: u64, answer: Option<String>) {
        if let Some((started, tx)) = self.pending.lock().unwrap().take()
            && started == generation
            && let Some(answer) = answer
        {
            let _ = tx.send(answer);
        }
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
        let (dialogs, generation, weak) = (self.clone(), generation.clone(), weak.clone());
        slint::invoke_from_event_loop(move || {
            if generation.load(Ordering::SeqCst) != started {
                return;
            }
            let Some(ui) = weak.upgrade() else { return };
            *dialogs.pending.lock().unwrap() = Some((started, tx));
            if file {
                ui.set_send_file_path(spec.label.into());
                ui.set_send_file_host(spec.description.into());
            } else {
                ui.set_send_prompt_title(spec.label.into());
                ui.set_send_prompt_description(spec.description.into());
            }
            ui.set_prompt_text(spec.default.into());
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
            *dialogs.pending.lock().unwrap() = Some((1, tx));
            dialogs.reply(generation, answer);
            assert_eq!(rx.recv().ok(), expected);
        }
        let (tx, rx) = mpsc::channel();
        *dialogs.pending.lock().unwrap() = Some((1, tx));
        dialogs.cancel();
        assert!(rx.recv().is_err());
        dialogs.reply(1, Some("too late".into()));
    }
}
