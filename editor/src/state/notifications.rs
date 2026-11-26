use super::Editor;
use crate::documents::DocId;
use rune::Tx;
use rune_kit::{ToastEvent, ToastStack};
use std::time::Duration;

#[derive(Clone)]
pub(crate) enum NoticeAction {
    Compare(DocId),
    Rebuild,
}

pub(crate) struct Notice {
    pub(crate) text: String,
    pub(crate) action: Option<NoticeAction>,
}

#[derive(Default)]
pub(crate) struct Notifications {
    clock: f64,
    pub(crate) stack: ToastStack<Notice>,
}

impl Notifications {
    pub(crate) fn tick(&mut self, clock: f64, tx: &mut Tx) -> bool {
        self.clock = clock;
        let before = self.stack.entries().len();
        self.stack.step(ToastEvent::Tick { now: clock });
        if let Some(due) = self.stack.next_due() {
            tx.wake_after(Duration::from_secs_f64((due - clock).max(0.0)));
        }
        before != self.stack.entries().len()
    }

    pub(crate) fn dismiss(&mut self, id: u64) {
        self.stack.step(ToastEvent::Dismissed {
            id: Some(id),
            now: self.clock,
        });
    }
}

impl Editor {
    pub(in crate::state) fn notify(
        &mut self,
        key: String,
        text: String,
        action: Option<NoticeAction>,
    ) {
        self.notifications.stack.step(ToastEvent::Shown {
            key,
            options: Notice { text, action },
            secs: Some(8.0),
            now: self.notifications.clock,
        });
    }

    pub(crate) fn run_notice_action(&mut self, id: u64, action: &NoticeAction, tx: &mut Tx) {
        self.notifications.dismiss(id);
        match action {
            NoticeAction::Compare(doc_id) => self.compare_external(doc_id, tx),
            NoticeAction::Rebuild => self.request_voice_build(),
        }
    }
}
