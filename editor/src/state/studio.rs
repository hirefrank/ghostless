use super::{Editor, notifications::NoticeAction};
use crate::{
    bridge::{BridgeOp, BuildSummary, ChecksSummary, RulesSummary},
    error::EditorError,
};
use rune::Tx;
use std::{collections::HashMap, path::PathBuf};

pub(crate) const ALL_SECTIONS: &str = "All sections";

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReviewKind {
    Rules,
    Checks,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Decision {
    Confirm,
    Reject,
    Retire,
}

impl Decision {
    fn wire(self) -> String {
        match self {
            Self::Confirm => "confirm",
            Self::Reject => "reject",
            Self::Retire => "retire",
        }
        .into()
    }
}

struct Request {
    id: u64,
    generation: u64,
    project: PathBuf,
}

impl Request {
    fn matches(&self, id: u64, generation: u64, project: Option<&PathBuf>) -> bool {
        self.id == id && self.generation == generation && Some(&self.project) == project
    }
}

#[derive(Default)]
pub(crate) struct Studio {
    pub(crate) open: bool,
    pub(crate) tab: usize,
    pub(crate) show_history: [bool; 2],
    pub(crate) scope_picker: Option<String>,
    pub(crate) chosen_scopes: HashMap<String, String>,
    pub(crate) sections: Vec<String>,
    pub(crate) rules: Option<RulesSummary>,
    pub(crate) checks: Option<ChecksSummary>,
    pub(crate) error: Option<EditorError>,
    pub(crate) needs_rebuild: bool,
    rules_request: Option<Request>,
    checks_request: Option<Request>,
    decision: Option<Request>,
    build: Option<Request>,
}

impl Studio {
    pub(crate) fn pending_count(&self) -> usize {
        self.rules
            .as_ref()
            .map_or(0, |rules| rules.hypotheses.len())
            + self
                .checks
                .as_ref()
                .map_or(0, |checks| checks.pending.len())
    }

    pub(crate) fn deciding(&self) -> bool {
        self.decision.is_some()
    }

    pub(crate) fn building(&self) -> bool {
        self.build.is_some()
    }

    pub(crate) fn scope(&self, id: &str, initial: Option<&str>) -> String {
        self.chosen_scopes
            .get(id)
            .cloned()
            .or_else(|| initial.map(ToOwned::to_owned))
            .unwrap_or_else(|| ALL_SECTIONS.into())
    }
}

impl Editor {
    pub(crate) fn open_voice_studio(&mut self, tx: &mut Tx) {
        if self.voice_project.is_none() {
            return;
        }
        self.studio.open = true;
        self.studio.error = None;
        self.refresh_voice_review();
        tx.focus("voice-studio-heading", false);
    }

    pub(crate) fn close_voice_studio(&mut self, tx: &mut Tx) {
        self.studio.open = false;
        self.studio.scope_picker = None;
        self.focus_document(tx);
    }

    fn voice_request(&self, op: BridgeOp, project: PathBuf) -> Result<Request, EditorError> {
        let generation = self.workspace_generation();
        let id = self.bridge.request(None, generation, op)?;
        Ok(Request {
            id,
            generation,
            project,
        })
    }

    pub(in crate::state) fn refresh_voice_review(&mut self) {
        let Some(project) = self.voice_project.clone() else {
            return;
        };
        self.studio.sections = crate::fs_io::read_utf8(&project.join("voice-stats.json"))
            .ok()
            .and_then(|stats| serde_json::from_str::<crate::contracts::VoiceStats>(&stats).ok())
            .map(|stats| stats.sections.into_keys().collect())
            .unwrap_or_default();
        let rules = self.voice_request(
            BridgeOp::RulesList {
                project: project.clone(),
            },
            project.clone(),
        );
        let checks = self.voice_request(
            BridgeOp::ChecksList {
                project: project.clone(),
            },
            project,
        );
        match rules {
            Ok(request) => self.studio.rules_request = Some(request),
            Err(error) => self.studio.error = Some(error),
        }
        match checks {
            Ok(request) => self.studio.checks_request = Some(request),
            Err(error) => self.studio.error = Some(error),
        }
    }

    pub(crate) fn decide_voice_item(
        &mut self,
        kind: ReviewKind,
        decision: Decision,
        id: &str,
        current_scope: Option<&str>,
    ) {
        if !self.studio.open
            || self.studio.deciding()
            || (kind == ReviewKind::Checks && decision == Decision::Retire)
        {
            return;
        }
        let Some(project) = self.voice_project.clone() else {
            return;
        };
        let scope = self.studio.scope(id, current_scope);
        let confirm = decision == Decision::Confirm;
        let global = confirm && scope == ALL_SECTIONS;
        let section = (confirm && !global).then_some(scope);
        let action = decision.wire();
        let op = match kind {
            ReviewKind::Rules => BridgeOp::RulesDecide {
                project: project.clone(),
                action,
                rule_id: id.into(),
                section,
                global,
            },
            ReviewKind::Checks => BridgeOp::ChecksDecide {
                project: project.clone(),
                action,
                target: id.into(),
                section,
                global,
                text: None,
            },
        };
        match self.voice_request(op, project) {
            Ok(request) => {
                self.studio.decision = Some(request);
                self.studio.error = None;
                self.studio.scope_picker = None;
            }
            Err(error) => self.studio.error = Some(error),
        }
    }

    fn accepts_voice_reply(&self, request: Option<&Request>, id: u64) -> bool {
        request.is_some_and(|request| {
            request.matches(id, self.workspace_generation(), self.voice_project.as_ref())
        })
    }

    pub(in crate::state) fn apply_rules_list(
        &mut self,
        id: u64,
        result: Result<RulesSummary, EditorError>,
    ) {
        if !self.accepts_voice_reply(self.studio.rules_request.as_ref(), id) {
            return;
        }
        self.studio.rules_request = None;
        match result {
            Ok(list) => self.studio.rules = Some(list),
            Err(error) => self.studio.error = Some(error),
        }
    }

    pub(in crate::state) fn apply_checks_list(
        &mut self,
        id: u64,
        result: Result<ChecksSummary, EditorError>,
    ) {
        if !self.accepts_voice_reply(self.studio.checks_request.as_ref(), id) {
            return;
        }
        self.studio.checks_request = None;
        match result {
            Ok(list) => self.studio.checks = Some(list),
            Err(error) => self.studio.error = Some(error),
        }
    }

    pub(in crate::state) fn apply_voice_decision(
        &mut self,
        id: u64,
        result: Result<(), EditorError>,
    ) {
        if !self.accepts_voice_reply(self.studio.decision.as_ref(), id) {
            return;
        }
        self.studio.decision = None;
        match result {
            Ok(_) => {
                self.studio.chosen_scopes.clear();
                self.studio.needs_rebuild = true;
                self.invalidate_voice_analysis();
                self.refresh_voice_review();
                self.notify(
                    "voice-decision".into(),
                    "Decision saved. Rebuild the voice guide to apply it.".into(),
                    Some(NoticeAction::Rebuild),
                );
            }
            Err(error) => self.studio.error = Some(error),
        }
    }

    pub(crate) fn request_voice_build(&mut self) {
        if self.studio.building() {
            return;
        }
        let Some(project) = self.voice_project.clone() else {
            self.status = "Open a voice project before building".into();
            return;
        };
        match self.voice_request(
            BridgeOp::Build {
                project: project.clone(),
            },
            project,
        ) {
            Ok(request) => {
                self.studio.build = Some(request);
                self.studio.error = None;
                self.status = "Building voice guide…".into();
            }
            Err(error) => self.status = error.to_string(),
        }
    }

    pub(in crate::state) fn apply_voice_build(
        &mut self,
        id: u64,
        result: Result<BuildSummary, EditorError>,
    ) {
        if !self.accepts_voice_reply(self.studio.build.as_ref(), id) {
            return;
        }
        self.studio.build = None;
        match result {
            Ok(_) => {
                self.voice_rebuilt();
                self.status = "Voice guide rebuilt".into();
                self.notify("voice-built".into(), self.status.clone(), None);
            }
            Err(error) => {
                self.status = format!("Build failed: {error}");
                self.studio.error = Some(error);
            }
        }
    }

    pub(in crate::state) fn voice_rebuilt(&mut self) {
        self.studio.needs_rebuild = false;
        self.invalidate_voice_analysis();
        self.refresh_voice_review();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{bridge::BridgeBackend, prefs, test_util::TestDir, worker::WriteOutcome};
    use serde_json::{Value, json};
    use std::{sync::mpsc, time::Duration};

    struct Record(mpsc::Sender<Value>);

    impl BridgeBackend for Record {
        fn call(&mut self, method: &str, params: &Value) -> Result<Value, EditorError> {
            if method == "checks_decide" {
                self.0.send(params.clone()).unwrap();
            }
            Ok(json!({"id":"check:opener","status":"confirmed"}))
        }
    }

    #[test]
    fn global_decision_is_explicit_and_stale_reply_is_dropped_after_switch() {
        let old = TestDir::new("studio-old");
        let new = TestDir::new("studio-new");
        let (sent, received) = mpsc::channel();
        let mut editor = Editor::with_handlers_backend(
            crate::session::Session {
                project: old.to_path_buf(),
                id: "2026-01-02-notes".into(),
                source_name: "draft.md".into(),
                original: "Before".into(),
                after: "Working".into(),
                notes: String::new(),
                before_sha256: "test".into(),
                finished: false,
            },
            prefs::EditorStateStore::default(),
            Box::new(|_, _| Ok(String::new())),
            Box::new(|_| Ok(WriteOutcome::Local)),
            Box::new(|| {}),
            Some(Box::new(Record(sent))),
        );
        editor.voice_project = Some(old.to_path_buf());
        editor.studio.open = true;
        editor
            .studio
            .chosen_scopes
            .insert("check:opener".into(), ALL_SECTIONS.into());
        editor.decide_voice_item(
            ReviewKind::Checks,
            Decision::Confirm,
            "check:opener",
            Some("notes"),
        );

        let params = received.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(params["global"], true);
        assert!(params["section"].is_null());

        let reply = editor.bridge.recv_timeout(Duration::from_secs(5)).unwrap();
        editor.voice_project = Some(new.to_path_buf());
        editor.restart_watcher();
        editor.apply_bridge_reply(reply);
        assert!(!editor.studio.needs_rebuild);
        assert!(editor.notifications.stack.entries().is_empty());
    }
}
