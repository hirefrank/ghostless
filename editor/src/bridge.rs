//! Persistent client for the local `ghostless bridge` NDJSON worker.
//!
//! Analysis calls stay on this thread. Saves and finishes stay on
//! [`crate::worker::Worker`] so a slow judge cannot block a write.

use std::{
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
};

#[cfg(test)]
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::{cli, documents::DocId, worker::WakeFn};

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct JudgeCheckMetric {
    pub metric: String,
    pub label: String,
    pub target: f64,
    pub actual: f64,
    pub low: f64,
    pub high: f64,
    pub status: String,
    pub penalty: i32,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub(crate) struct BannedHit {
    pub tell: String,
    pub count: usize,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub(crate) struct ConfirmedRuleHit {
    pub id: String,
    pub word: String,
    pub scope: Option<String>,
    pub count: usize,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub(crate) struct StructureHit {
    pub kind: String,
    pub excerpt: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConfidenceInfo {
    pub level: String,
    pub trustworthy: bool,
    pub pieces: usize,
    pub words: usize,
    pub written_words: usize,
    pub sentences: usize,
    pub note: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub(crate) struct LiveCheckResult {
    pub id: String,
    pub text: String,
    pub span: String,
    pub polarity: String,
    pub section: Option<String>,
    pub pass: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct JudgeReport {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub score: i32,
    #[serde(default)]
    pub fail_under: i32,
    #[serde(default)]
    pub section: String,
    #[serde(default)]
    pub checks: Vec<JudgeCheckMetric>,
    #[serde(default)]
    pub banned: Vec<BannedHit>,
    #[serde(default)]
    pub confirmed_rules: Vec<ConfirmedRuleHit>,
    #[serde(default)]
    pub structure: Vec<StructureHit>,
    #[serde(default)]
    pub placeholders: Vec<String>,
    pub confidence: Option<ConfidenceInfo>,
    #[serde(default)]
    pub low_confidence: bool,
    #[serde(default)]
    pub stale: bool,
    #[serde(default)]
    pub stale_sources: Vec<String>,
    #[serde(default)]
    pub available_sections: Vec<String>,
    #[serde(default)]
    pub confirmed_checks: Vec<LiveCheckResult>,
    #[serde(default)]
    pub needs_build: bool,
    pub error: Option<String>,
}

impl JudgeReport {
    pub(crate) fn from_error(error: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(error.into()),
            ..Self::default()
        }
    }

    pub(crate) fn gap_count(&self) -> usize {
        self.checks.iter().filter(|check| check.status != "ok").count()
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BuildSummary {
    pub gold_pieces: i64,
    #[serde(default)]
    pub counts: Value,
    #[serde(default)]
    pub bronze_format_only: bool,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub generated_at: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionStartResult {
    pub id: String,
    pub path: String,
    pub before_sha256: String,
    #[serde(default)]
    pub source_draft_name: String,
    #[serde(default)]
    pub section: Option<String>,
    #[serde(default)]
    pub origin: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub(crate) struct SaveNoteResult {
    pub path: String,
    pub created: bool,
    pub appended: bool,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(crate) enum BridgeOp {
    JudgeText {
        project: PathBuf,
        text: String,
        section: Option<String>,
    },
    Build {
        project: PathBuf,
    },
    SessionsList {
        project: PathBuf,
    },
    SessionStart {
        project: PathBuf,
        draft: PathBuf,
        section: Option<String>,
        origin: String,
        sources: Vec<String>,
    },
    SessionFinish {
        project: PathBuf,
        session: String,
        note: Option<String>,
        checks: Vec<String>,
        rebuild: bool,
        promote: bool,
        final_path: Option<PathBuf>,
    },
    RulesList {
        project: PathBuf,
    },
    RulesDecide {
        project: PathBuf,
        action: String,
        rule_id: String,
        section: Option<String>,
    },
    ChecksList {
        project: PathBuf,
    },
    ChecksDecide {
        project: PathBuf,
        action: String,
        target: String,
        section: Option<String>,
        text: Option<String>,
    },
    SaveNote {
        project: PathBuf,
        kind: String,
        text: String,
    },
    Promote {
        project: PathBuf,
        slug: String,
        demote: bool,
    },
}

#[derive(Clone, Debug)]
pub(crate) enum BridgeReply {
    Judge {
        doc_id: DocId,
        generation: u64,
        section: String,
        result: Box<JudgeReport>,
    },
    Build {
        doc_id: Option<DocId>,
        result: Result<BuildSummary, String>,
    },
    SessionsList {
        result: Result<Vec<Value>, String>,
    },
    SessionStart {
        doc_id: Option<DocId>,
        result: Result<SessionStartResult, String>,
    },
    SessionFinish {
        doc_id: Option<DocId>,
        result: Result<Value, String>,
    },
    RulesList {
        result: Result<Value, String>,
    },
    RulesDecide {
        result: Result<Value, String>,
    },
    ChecksList {
        result: Result<Value, String>,
    },
    ChecksDecide {
        result: Result<Value, String>,
    },
    SaveNote {
        result: Result<SaveNoteResult, String>,
    },
    Promote {
        result: Result<Value, String>,
    },
}

pub(crate) trait BridgeBackend: Send {
    fn call(&mut self, method: &str, params: &Value) -> Result<Value, String>;
}

struct Job {
    id: u64,
    doc_id: Option<DocId>,
    generation: u64,
    section_label: String,
    op: BridgeOp,
}

enum Transport {
    Process {
        launcher: Box<dyn Fn() -> Command + Send>,
        live: Option<LiveProcess>,
    },
    Mock(Box<dyn BridgeBackend>),
}

struct LiveProcess {
    stdin: ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
}

struct Shared {
    stop: AtomicBool,
    child: Mutex<Option<Child>>,
}

pub(crate) struct BridgeClient {
    tx: Option<mpsc::Sender<Job>>,
    rx: mpsc::Receiver<BridgeReply>,
    join: Option<thread::JoinHandle<()>>,
    shared: Arc<Shared>,
    next_id: AtomicU64,
    pub(crate) submitted: Arc<AtomicU64>,
}

impl BridgeClient {
    pub(crate) fn spawn(wake: Arc<WakeFn>, backend: Option<Box<dyn BridgeBackend>>) -> Self {
        match backend.or_else(test_backend) {
            Some(backend) => Self::start(wake, Transport::Mock(backend)),
            None => Self::spawn_process(wake),
        }
    }

    pub(crate) fn spawn_process(wake: Arc<WakeFn>) -> Self {
        Self::spawn_with_launcher(wake, default_command)
    }

    pub(crate) fn spawn_with_launcher(
        wake: Arc<WakeFn>,
        launcher: impl Fn() -> Command + Send + 'static,
    ) -> Self {
        Self::start(
            wake,
            Transport::Process {
                launcher: Box::new(launcher),
                live: None,
            },
        )
    }

    fn start(wake: Arc<WakeFn>, transport: Transport) -> Self {
        let (job_tx, job_rx) = mpsc::channel::<Job>();
        let (rep_tx, rep_rx) = mpsc::channel::<BridgeReply>();
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            child: Mutex::new(None),
        });
        let shared_thread = Arc::clone(&shared);
        let submitted = Arc::new(AtomicU64::new(0));
        let join = thread::spawn(move || worker_loop(job_rx, rep_tx, wake, transport, shared_thread));
        Self {
            tx: Some(job_tx),
            rx: rep_rx,
            join: Some(join),
            shared,
            next_id: AtomicU64::new(1),
            submitted,
        }
    }

    pub(crate) fn request(
        &self,
        doc_id: Option<DocId>,
        generation: u64,
        op: BridgeOp,
    ) -> Result<(), String> {
        let Some(tx) = &self.tx else {
            return Err("Voice bridge stopped".into());
        };
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let section_label = match &op {
            BridgeOp::JudgeText { section, .. } => section.clone().unwrap_or_else(|| "all".into()),
            _ => String::new(),
        };
        self.submitted.fetch_add(1, Ordering::Relaxed);
        tx.send(Job {
            id,
            doc_id,
            generation,
            section_label,
            op,
        })
        .map_err(|_| "Voice bridge stopped".into())
    }

    pub(crate) fn try_recv(&self) -> Result<BridgeReply, mpsc::TryRecvError> {
        self.rx.try_recv()
    }

    #[cfg(test)]
    pub(crate) fn recv_timeout(&self, timeout: Duration) -> Result<BridgeReply, String> {
        self.rx.recv_timeout(timeout).map_err(|error| error.to_string())
    }

    pub(crate) fn shutdown(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(mut child) = self.shared.child.lock().unwrap().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.tx.take();
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for BridgeClient {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn worker_loop(
    jobs: mpsc::Receiver<Job>,
    replies: mpsc::Sender<BridgeReply>,
    wake: Arc<WakeFn>,
    mut transport: Transport,
    shared: Arc<Shared>,
) {
    while let Ok(job) = jobs.recv() {
        if shared.stop.load(Ordering::Relaxed) {
            break;
        }
        let reply = execute(&mut transport, &shared, job);
        if replies.send(reply).is_err() {
            break;
        }
        wake();
    }
    transport.shutdown(&shared);
}

fn execute(transport: &mut Transport, shared: &Shared, job: Job) -> BridgeReply {
    let payload = match job.payload() {
        Ok(payload) => payload,
        Err(error) => return job.fail(error),
    };
    match roundtrip(transport, shared, &payload) {
        Ok(line) => job.decode(&line),
        Err(error) => job.fail(error),
    }
}

/// One automatic retry after a crash or EOF. A second failure is returned.
fn roundtrip(transport: &mut Transport, shared: &Shared, payload: &str) -> Result<String, String> {
    match transport.exchange(shared, payload) {
        Ok(line) => Ok(line),
        Err(error) if transport.is_transport_failure() => {
            if shared.stop.load(Ordering::Relaxed) {
                return Err(error);
            }
            match transport.exchange(shared, payload) {
                Ok(line) => Ok(line),
                Err(retry) => Err(format!("Voice bridge unavailable: {retry}")),
            }
        }
        Err(error) => Err(error),
    }
}

impl Transport {
    fn is_transport_failure(&self) -> bool {
        matches!(self, Transport::Process { .. })
    }

    fn exchange(&mut self, shared: &Shared, payload: &str) -> Result<String, String> {
        if shared.stop.load(Ordering::Relaxed) {
            return Err("Voice bridge stopped".into());
        }
        match self {
            Transport::Mock(backend) => mock_exchange(backend.as_mut(), payload),
            Transport::Process { launcher, live } => {
                if live.is_none() {
                    *live = Some(spawn_process(launcher.as_ref(), shared)?);
                }
                let current = live.as_mut().expect("process spawned");
                match write_and_read(current, payload) {
                    Ok(line) => Ok(line),
                    Err(error) => {
                        shutdown_live(live.take(), shared);
                        Err(error)
                    }
                }
            }
        }
    }

    fn shutdown(&mut self, shared: &Shared) {
        if let Transport::Process { live, .. } = self {
            shutdown_live(live.take(), shared);
        }
    }
}

fn mock_exchange(backend: &mut dyn BridgeBackend, payload: &str) -> Result<String, String> {
    let value: Value = serde_json::from_str(payload).map_err(|error| error.to_string())?;
    let id = value.get("id").cloned().unwrap_or(Value::Null);
    let method = value
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let params = value.get("params").cloned().unwrap_or_else(|| json!({}));
    let body = match backend.call(method, &params) {
        Ok(result) => json!({"id": id, "ok": true, "result": result}),
        Err(error) => json!({"id": id, "ok": false, "error": error}),
    };
    Ok(body.to_string())
}

fn default_command() -> Command {
    let mut command = cli::cli_command();
    command
        .arg("bridge")
        .env("GHOSTLESS_NO_UPDATE", "1")
        .env("PYTHONUTF8", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn spawn_process(
    launcher: &dyn Fn() -> Command,
    shared: &Shared,
) -> Result<LiveProcess, String> {
    if shared.stop.load(Ordering::Relaxed) {
        return Err("Voice bridge stopped".into());
    }
    let mut command = launcher();
    let mut child = command
        .spawn()
        .map_err(|error| format!("Cannot launch ghostless bridge: {error}"))?;
    let stdin = child
        .stdin
        .take()
        .ok_or("Cannot open bridge input")?;
    let stdout = child
        .stdout
        .take()
        .ok_or("Cannot open bridge output")?;
    if let Some(stderr) = child.stderr.take() {
        thread::spawn(move || {
            let mut sink = String::new();
            let _ = std::io::Read::read_to_string(&mut BufReader::new(stderr), &mut sink);
        });
    }
    *shared.child.lock().unwrap() = Some(child);
    Ok(LiveProcess {
        stdin,
        stdout: BufReader::new(stdout),
    })
}

fn write_and_read(live: &mut LiveProcess, payload: &str) -> Result<String, String> {
    live.stdin
        .write_all(payload.as_bytes())
        .and_then(|_| live.stdin.write_all(b"\n"))
        .and_then(|_| live.stdin.flush())
        .map_err(|error| format!("Bridge write failed: {error}"))?;
    let mut line = String::new();
    let read = live
        .stdout
        .read_line(&mut line)
        .map_err(|error| format!("Bridge read failed: {error}"))?;
    if read == 0 {
        return Err("Bridge closed its output".into());
    }
    Ok(line)
}

fn shutdown_live(live: Option<LiveProcess>, shared: &Shared) {
    drop(live);
    if let Some(mut child) = shared.child.lock().unwrap().take() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

impl Job {
    fn payload(&self) -> Result<String, String> {
        let (method, params) = self.op.params()?;
        Ok(json!({"id": self.id, "method": method, "params": params}).to_string())
    }

    fn decode(self, line: &str) -> BridgeReply {
        let envelope: Envelope = match serde_json::from_str(line) {
            Ok(envelope) => envelope,
            Err(error) => return self.fail(format!("Invalid bridge response: {error}")),
        };
        if envelope.id != Some(self.id) {
            return self.fail("Bridge response did not match the request".to_owned());
        }
        if !envelope.ok {
            return self.fail(envelope.error.unwrap_or_else(|| "Bridge request failed".into()));
        }
        let result = envelope.result.unwrap_or(Value::Null);
        match self.op {
            BridgeOp::JudgeText { .. } => {
                let doc_id = self.doc_id.clone().unwrap_or(DocId::File(PathBuf::new()));
                let report = serde_json::from_value::<JudgeReport>(result)
                    .unwrap_or_else(|error| JudgeReport::from_error(format!("Invalid judge report: {error}")));
                BridgeReply::Judge {
                    doc_id,
                    generation: self.generation,
                    section: self.section_label,
                    result: Box::new(report),
                }
            }
            BridgeOp::Build { .. } => BridgeReply::Build {
                doc_id: self.doc_id,
                result: parse_value(result),
            },
            BridgeOp::SessionsList { .. } => BridgeReply::SessionsList {
                result: parse_value(result),
            },
            BridgeOp::SessionStart { .. } => BridgeReply::SessionStart {
                doc_id: self.doc_id,
                result: parse_value(result),
            },
            BridgeOp::SessionFinish { .. } => BridgeReply::SessionFinish {
                doc_id: self.doc_id,
                result: Ok(result),
            },
            BridgeOp::RulesList { .. } => BridgeReply::RulesList { result: Ok(result) },
            BridgeOp::RulesDecide { .. } => BridgeReply::RulesDecide { result: Ok(result) },
            BridgeOp::ChecksList { .. } => BridgeReply::ChecksList { result: Ok(result) },
            BridgeOp::ChecksDecide { .. } => BridgeReply::ChecksDecide { result: Ok(result) },
            BridgeOp::SaveNote { .. } => BridgeReply::SaveNote { result: parse_value(result) },
            BridgeOp::Promote { .. } => BridgeReply::Promote { result: Ok(result) },
        }
    }

    fn fail(self, error: String) -> BridgeReply {
        match self.op {
            BridgeOp::JudgeText { .. } => BridgeReply::Judge {
                doc_id: self.doc_id.unwrap_or(DocId::File(PathBuf::new())),
                generation: self.generation,
                section: self.section_label,
                result: Box::new(JudgeReport::from_error(error)),
            },
            BridgeOp::Build { .. } => BridgeReply::Build {
                doc_id: self.doc_id,
                result: Err(error),
            },
            BridgeOp::SessionsList { .. } => BridgeReply::SessionsList { result: Err(error) },
            BridgeOp::SessionStart { .. } => BridgeReply::SessionStart {
                doc_id: self.doc_id,
                result: Err(error),
            },
            BridgeOp::SessionFinish { .. } => BridgeReply::SessionFinish {
                doc_id: self.doc_id,
                result: Err(error),
            },
            BridgeOp::RulesList { .. } => BridgeReply::RulesList { result: Err(error) },
            BridgeOp::RulesDecide { .. } => BridgeReply::RulesDecide { result: Err(error) },
            BridgeOp::ChecksList { .. } => BridgeReply::ChecksList { result: Err(error) },
            BridgeOp::ChecksDecide { .. } => BridgeReply::ChecksDecide { result: Err(error) },
            BridgeOp::SaveNote { .. } => BridgeReply::SaveNote { result: Err(error) },
            BridgeOp::Promote { .. } => BridgeReply::Promote { result: Err(error) },
        }
    }
}

impl BridgeOp {
    fn params(&self) -> Result<(&'static str, Value), String> {
        let path = |value: &Path| -> Result<String, String> {
            value
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("Path is not UTF-8: {}", value.display()))
        };
        Ok(match self {
            BridgeOp::JudgeText {
                project,
                text,
                section,
            } => (
                "judge_text",
                json!({"project": path(project)?, "text": text, "section": section}),
            ),
            BridgeOp::Build { project } => ("build", json!({"project": path(project)?})),
            BridgeOp::SessionsList { project } => ("sessions_list", json!({"project": path(project)?})),
            BridgeOp::SessionStart {
                project,
                draft,
                section,
                origin,
                sources,
            } => (
                "session_start",
                json!({
                    "project": path(project)?,
                    "draft": path(draft)?,
                    "section": section,
                    "origin": origin,
                    "sources": sources,
                }),
            ),
            BridgeOp::SessionFinish {
                project,
                session,
                note,
                checks,
                rebuild,
                promote,
                final_path,
            } => (
                "session_finish",
                json!({
                    "project": path(project)?,
                    "session": session,
                    "note": note,
                    "checks": checks,
                    "rebuild": rebuild,
                    "promote": promote,
                    "final": final_path.as_ref().map(|item| path(item)).transpose()?,
                }),
            ),
            BridgeOp::RulesList { project } => ("rules_list", json!({"project": path(project)?})),
            BridgeOp::RulesDecide {
                project,
                action,
                rule_id,
                section,
            } => (
                "rules_decide",
                json!({
                    "project": path(project)?,
                    "action": action,
                    "ruleId": rule_id,
                    "section": section,
                }),
            ),
            BridgeOp::ChecksList { project } => ("checks_list", json!({"project": path(project)?})),
            BridgeOp::ChecksDecide {
                project,
                action,
                target,
                section,
                text,
            } => (
                "checks_decide",
                json!({
                    "project": path(project)?,
                    "action": action,
                    "target": target,
                    "section": section,
                    "text": text,
                }),
            ),
            BridgeOp::SaveNote { project, kind, text } => (
                "save_note",
                json!({"project": path(project)?, "kind": kind, "text": text}),
            ),
            BridgeOp::Promote {
                project,
                slug,
                demote,
            } => (
                "promote",
                json!({"project": path(project)?, "slug": slug, "demote": demote}),
            ),
        })
    }
}

#[derive(Deserialize)]
struct Envelope {
    id: Option<u64>,
    #[serde(default)]
    ok: bool,
    result: Option<Value>,
    error: Option<String>,
}

fn parse_value<T: for<'de> Deserialize<'de>>(value: Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|error| format!("Unexpected bridge payload: {error}"))
}

fn test_backend() -> Option<Box<dyn BridgeBackend>> {
    #[cfg(test)]
    {
        Some(Box::new(NullBackend))
    }
    #[cfg(not(test))]
    {
        None
    }
}

#[cfg(test)]
struct NullBackend;

#[cfg(test)]
impl BridgeBackend for NullBackend {
    fn call(&mut self, method: &str, _params: &Value) -> Result<Value, String> {
        Ok(match method {
            "judge_text" => json!({
                "ok": false,
                "score": 0,
                "needsBuild": true,
                "error": "No voice-stats.json yet. Run build first."
            }),
            "sessions_list" => json!([]),
            "save_note" => json!({"path": "STYLE.md", "created": false, "appended": true}),
            _ => json!({}),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    struct RecordingBackend {
        calls: Arc<Mutex<Vec<(String, Value)>>>,
        report: Value,
        hold: Option<mpsc::Receiver<()>>,
    }

    impl BridgeBackend for RecordingBackend {
        fn call(&mut self, method: &str, params: &Value) -> Result<Value, String> {
            self.calls
                .lock()
                .unwrap()
                .push((method.to_owned(), params.clone()));
            if let Some(hold) = &self.hold {
                let _ = hold.recv_timeout(Duration::from_secs(3));
            }
            Ok(self.report.clone())
        }
    }

    fn sample_report() -> Value {
        json!({
            "ok": true,
            "score": 86,
            "failUnder": 70,
            "section": "all",
            "checks": [{
                "metric": "medianSentenceLength",
                "label": "median sentence length",
                "target": 14,
                "actual": 12,
                "low": 9.0,
                "high": 18.0,
                "status": "ok",
                "penalty": 0
            }],
            "banned": [],
            "confirmedRules": [],
            "structure": [],
            "placeholders": [],
            "confidence": {
                "level": "high",
                "trustworthy": true,
                "pieces": 14,
                "words": 2000,
                "writtenWords": 1800,
                "sentences": 40,
                "note": "Enough writing to trust the score."
            },
            "lowConfidence": false,
            "stale": false,
            "staleSources": [],
            "availableSections": ["notes"],
            "confirmedChecks": [],
            "needsBuild": false
        })
    }

    #[test]
    fn every_bridge_op_names_its_method() {
        let project = PathBuf::from("/voice");
        let ops = vec![
            BridgeOp::JudgeText { project: project.clone(), text: "Hi".into(), section: Some("notes".into()) },
            BridgeOp::Build { project: project.clone() },
            BridgeOp::SessionsList { project: project.clone() },
            BridgeOp::SessionStart {
                project: project.clone(),
                draft: PathBuf::from("/draft.md"),
                section: None,
                origin: "ai-first".into(),
                sources: vec![],
            },
            BridgeOp::SessionFinish {
                project: project.clone(),
                session: "2026-01-02".into(),
                note: None,
                checks: vec![],
                rebuild: false,
                promote: false,
                final_path: None,
            },
            BridgeOp::RulesList { project: project.clone() },
            BridgeOp::RulesDecide { project: project.clone(), action: "confirm".into(), rule_id: "cut:very".into(), section: None },
            BridgeOp::ChecksList { project: project.clone() },
            BridgeOp::ChecksDecide { project: project.clone(), action: "reject".into(), target: "pair".into(), section: None, text: None },
            BridgeOp::SaveNote { project: project.clone(), kind: "style".into(), text: "Short sentences.".into() },
            BridgeOp::Promote { project, slug: "demo".into(), demote: false },
        ];
        let methods = [
            "judge_text",
            "build",
            "sessions_list",
            "session_start",
            "session_finish",
            "rules_list",
            "rules_decide",
            "checks_list",
            "checks_decide",
            "save_note",
            "promote",
        ];
        for (op, method) in ops.into_iter().zip(methods) {
            let (_name, params) = op.params().unwrap();
            let job = Job { id: 7, doc_id: None, generation: 1, section_label: String::new(), op };
            let line = job.payload().unwrap();
            let value: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(value["method"], method);
            assert!(params.is_object());
            assert_eq!(value["id"], 7);
        }
    }

    #[test]
    fn mock_judge_roundtrip_uses_the_backend_once() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let backend = RecordingBackend {
            calls: Arc::clone(&calls),
            report: sample_report(),
            hold: None,
        };
        let client = BridgeClient::spawn(Arc::new(Box::new(|| {})), Some(Box::new(backend)));
        client
            .request(
                Some(DocId::File(PathBuf::from("/draft.md"))),
                3,
                BridgeOp::JudgeText {
                    project: PathBuf::from("/voice"),
                    text: "Hello".into(),
                    section: None,
                },
            )
            .unwrap();
        let reply = client.recv_timeout(Duration::from_secs(2)).unwrap();
        match reply {
            BridgeReply::Judge { generation, result, .. } => {
                assert_eq!(generation, 3);
                assert_eq!(result.score, 86);
                assert_eq!(result.confidence.unwrap().written_words, 1800);
            }
            other => panic!("unexpected reply: {other:?}"),
        }
        assert_eq!(calls.lock().unwrap().len(), 1);
        assert_eq!(calls.lock().unwrap()[0].0, "judge_text");
    }

    #[test]
    fn crashed_bridge_retries_once_then_reports_the_error() {
        let spawns = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&spawns);
        let client = BridgeClient::spawn_with_launcher(Arc::new(Box::new(|| {})), move || {
            seen.fetch_add(1, Ordering::SeqCst);
            failing_command()
        });
        client
            .request(None, 0, BridgeOp::Build { project: PathBuf::from("/voice") })
            .unwrap();
        let reply = client.recv_timeout(Duration::from_secs(2)).unwrap();
        match reply {
            BridgeReply::Build { result, .. } => assert!(result.is_err(), "{result:?}"),
            other => panic!("unexpected reply: {other:?}"),
        }
        assert_eq!(spawns.load(Ordering::SeqCst), 2);
    }

    fn failing_command() -> Command {
        let mut command = Command::new(if cfg!(windows) { "cmd" } else { "sh" });
        if cfg!(windows) {
            command.args(["/C", "exit", "1"]);
        } else {
            command.args(["-c", "exit 1"]);
        }
        command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        command
    }

    #[test]
    fn live_bridge_deserializes_sample_drafts_and_exits() {
        let _lock = crate::ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures/sample-voice");
        let root = std::env::temp_dir().join(format!("ghostless-bridge-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let project = root.join("voice");
        copy_dir(&fixture, &project).unwrap();
        let client = BridgeClient::spawn_process(Arc::new(Box::new(|| {})));
        client
            .request(None, 0, BridgeOp::Build { project: project.clone() })
            .unwrap();
        match client.recv_timeout(Duration::from_secs(30)).unwrap() {
            BridgeReply::Build { result, .. } => {
                let summary = result.expect("build");
                assert!(summary.gold_pieces > 0);
            }
            other => panic!("unexpected build reply: {other:?}"),
        }
        for name in ["ai-ish.md", "closer.md"] {
            let text = std::fs::read_to_string(project.join("drafts").join(name)).unwrap();
            client
                .request(
                    Some(DocId::File(project.join("drafts").join(name))),
                    1,
                    BridgeOp::JudgeText { project: project.clone(), text, section: None },
                )
                .unwrap();
            match client.recv_timeout(Duration::from_secs(30)).unwrap() {
                BridgeReply::Judge { result, .. } => {
                    assert!(result.error.is_none(), "{:?}", result.error);
                    assert!(!result.checks.is_empty(), "{name} produced no checks");
                    assert!(result.confidence.is_some());
                }
                other => panic!("unexpected judge reply: {other:?}"),
            }
        }
        drop(client);
        std::fs::remove_dir_all(root).unwrap();
    }

    fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            let dest = to.join(entry.file_name());
            if entry.file_type()?.is_dir() {
                copy_dir(&entry.path(), &dest)?;
            } else {
                std::fs::copy(entry.path(), dest)?;
            }
        }
        Ok(())
    }
}
