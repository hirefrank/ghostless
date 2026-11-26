use super::{BridgeOp, BridgeReply, Envelope, Job, JudgeReport, parse_value};
use crate::{documents::DocId, error::EditorError};
use serde::Serialize;
use std::path::PathBuf;

impl Job {
    pub(super) fn payload(&self) -> Result<String, EditorError> {
        match &self.op {
            BridgeOp::SessionFinish { session, .. }
                if !crate::session::valid_session_id(session) =>
            {
                return Err(EditorError::InvalidSessionId(session.clone()));
            }
            BridgeOp::RulesDecide {
                global: true,
                section: Some(_),
                ..
            }
            | BridgeOp::ChecksDecide {
                global: true,
                section: Some(_),
                ..
            } => {
                return Err(EditorError::Bridge(
                    "A global decision cannot also have a section".into(),
                ));
            }
            _ => {}
        }
        #[derive(Serialize)]
        struct Request<'a> {
            id: u64,
            #[serde(flatten)]
            op: &'a BridgeOp,
        }
        serde_json::to_string(&Request {
            id: self.id,
            op: &self.op,
        })
        .map_err(|error| EditorError::Json {
            context: format!("Cannot encode bridge request: {error}"),
        })
    }

    pub(super) fn decode(self, line: &str) -> BridgeReply {
        let envelope: Envelope = match serde_json::from_str(line) {
            Ok(envelope) => envelope,
            Err(error) => return self.fail(format!("Invalid bridge response: {error}")),
        };
        let result = match envelope.into_payload(self.id) {
            Ok(result) => result,
            Err(error) => return self.fail(error),
        };
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
                result: parse_value(result),
            },
            BridgeOp::RulesList { .. } => BridgeReply::RulesList {
                request_id: self.id,
                result: parse_value(result),
            },
            BridgeOp::RulesDecide { .. } => BridgeReply::RulesDecide {
                request_id: self.id,
                result: parse_value(result),
            },
            BridgeOp::ChecksList { .. } => BridgeReply::ChecksList {
                request_id: self.id,
                result: parse_value(result),
            },
            BridgeOp::ChecksDecide { .. } => BridgeReply::ChecksDecide {
                request_id: self.id,
                result: parse_value(result),
            },
            BridgeOp::SaveNote { .. } => BridgeReply::SaveNote {
                result: parse_value(result),
            },
            BridgeOp::Promote { .. } => BridgeReply::Promote {
                result: parse_value(result),
            },
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
