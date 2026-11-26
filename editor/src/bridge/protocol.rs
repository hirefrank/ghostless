use super::{BridgeOp, BridgeReply, Envelope, Job, JudgeReport, parse_value};
use crate::{documents::DocId, error::EditorError};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

impl Job {
    pub(super) fn payload(&self) -> Result<String, EditorError> {
        let (method, params) = self.op.params()?;
        Ok(json!({"id": self.id, "method": method, "params": params}).to_string())
    }

    pub(super) fn decode(self, line: &str) -> BridgeReply {
        let envelope: Envelope = match serde_json::from_str(line) {
            Ok(envelope) => envelope,
            Err(error) => return self.fail(format!("Invalid bridge response: {error}")),
        };
        if envelope.id != Some(self.id) {
            return self.fail("Bridge response did not match the request".to_owned());
        }
        if !envelope.ok {
            return self.fail(
                envelope
                    .error
                    .unwrap_or_else(|| "Bridge request failed".into()),
            );
        }
        let result = envelope.result.unwrap_or(Value::Null);
        match self.op {
            BridgeOp::JudgeText { .. } => {
                let doc_id = self.doc_id.clone().unwrap_or(DocId::File(PathBuf::new()));
                let report =
                    serde_json::from_value::<JudgeReport>(result).unwrap_or_else(|error| {
                        JudgeReport::from_error(format!("Invalid judge report: {error}"))
                    });
                BridgeReply::Judge {
                    request_id: self.id,
                    doc_id,
                    generation: self.generation,
                    section: self.section_label,
                    result: Box::new(report),
                }
            }
            BridgeOp::Build { .. } => BridgeReply::Build {
                request_id: self.id,
                result: parse_value(result),
            },
            BridgeOp::SessionsList { .. } => BridgeReply::SessionsList {
                result: parse_value(result),
            },
            BridgeOp::SessionStart { .. } => BridgeReply::SessionStart {
                doc_id: self.doc_id,
                generation: self.generation,
                result: parse_value(result),
            },
            BridgeOp::SessionFinish { .. } => BridgeReply::SessionFinish {
                doc_id: self.doc_id,
                result: Ok(result),
            },
            BridgeOp::RulesList { .. } => BridgeReply::RulesList {
                request_id: self.id,
                result: parse_value(result),
            },
            BridgeOp::RulesDecide { .. } => BridgeReply::RulesDecide {
                request_id: self.id,
                result: Ok(result),
            },
            BridgeOp::ChecksList { .. } => BridgeReply::ChecksList {
                request_id: self.id,
                result: parse_value(result),
            },
            BridgeOp::ChecksDecide { .. } => BridgeReply::ChecksDecide {
                request_id: self.id,
                result: Ok(result),
            },
            BridgeOp::SaveNote { .. } => BridgeReply::SaveNote {
                result: parse_value(result),
            },
            BridgeOp::Promote { .. } => BridgeReply::Promote { result: Ok(result) },
        }
    }

    pub(super) fn fail(self, error: impl Into<EditorError>) -> BridgeReply {
        let error = error.into();
        match self.op {
            BridgeOp::JudgeText { .. } => BridgeReply::Judge {
                request_id: self.id,
                doc_id: self.doc_id.unwrap_or(DocId::File(PathBuf::new())),
                generation: self.generation,
                section: self.section_label,
                result: Box::new(JudgeReport::from_error(error.to_string())),
            },
            BridgeOp::Build { .. } => BridgeReply::Build {
                request_id: self.id,
                result: Err(error),
            },
            BridgeOp::SessionsList { .. } => BridgeReply::SessionsList { result: Err(error) },
            BridgeOp::SessionStart { .. } => BridgeReply::SessionStart {
                doc_id: self.doc_id,
                generation: self.generation,
                result: Err(error),
            },
            BridgeOp::SessionFinish { .. } => BridgeReply::SessionFinish {
                doc_id: self.doc_id,
                result: Err(error),
            },
            BridgeOp::RulesList { .. } => BridgeReply::RulesList {
                request_id: self.id,
                result: Err(error),
            },
            BridgeOp::RulesDecide { .. } => BridgeReply::RulesDecide {
                request_id: self.id,
                result: Err(error),
            },
            BridgeOp::ChecksList { .. } => BridgeReply::ChecksList {
                request_id: self.id,
                result: Err(error),
            },
            BridgeOp::ChecksDecide { .. } => BridgeReply::ChecksDecide {
                request_id: self.id,
                result: Err(error),
            },
            BridgeOp::SaveNote { .. } => BridgeReply::SaveNote { result: Err(error) },
            BridgeOp::Promote { .. } => BridgeReply::Promote { result: Err(error) },
        }
    }
}

impl BridgeOp {
    pub(super) fn params(&self) -> Result<(&'static str, Value), EditorError> {
        let path = |value: &Path| -> Result<String, EditorError> {
            value.to_str().map(str::to_owned).ok_or_else(|| {
                EditorError::message(format!("Path is not UTF-8: {}", value.display()))
            })
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
            BridgeOp::SessionsList { project } => {
                ("sessions_list", json!({"project": path(project)?}))
            }
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
                global,
            } => (
                "rules_decide",
                json!({
                    "project": path(project)?,
                    "action": action,
                    "ruleId": rule_id,
                    "section": section,
                    "global": global,
                }),
            ),
            BridgeOp::ChecksList { project } => ("checks_list", json!({"project": path(project)?})),
            BridgeOp::ChecksDecide {
                project,
                action,
                target,
                section,
                global,
                text,
            } => (
                "checks_decide",
                json!({
                    "project": path(project)?,
                    "action": action,
                    "target": target,
                    "section": section,
                    "global": global,
                    "text": text,
                }),
            ),
            BridgeOp::SaveNote {
                project,
                kind,
                text,
            } => (
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
