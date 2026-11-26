//! Typed views of the project files the editor reads. Optional legacy metadata
//! stays readable. Saving still requires a verified baseline hash.
use crate::{error::EditorError, fs_io};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum ManifestKind {
    #[serde(rename = "ghostless")]
    Ghostless,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Manifest {
    #[serde(rename = "kind")]
    kind: ManifestKind,
    pub placeholder: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum SessionKind {
    #[serde(rename = "ghostless-edit-session")]
    EditSession,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum SessionStatus {
    Open,
    Final,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionMetadata {
    kind: SessionKind,
    pub id: Option<String>,
    pub version: Option<u32>,
    pub before_sha256: Option<String>,
    pub source_draft_name: Option<String>,
    pub created_at: Option<String>,
    pub section: Option<String>,
    pub origin: Option<String>,
    pub status: Option<SessionStatus>,
}

pub(crate) fn session_metadata(folder: &Path) -> Result<SessionMetadata, EditorError> {
    let raw = fs_io::read_utf8(&folder.join("session.json"))?;
    let metadata: SessionMetadata =
        serde_json::from_str(&raw).map_err(|error| EditorError::Json {
            context: format!(
                "Unsupported session format in {}: {error}",
                folder.display()
            ),
        })?;
    if metadata.version.is_some_and(|version| version != 1) {
        return Err("Unsupported session version".into());
    }
    Ok(metadata)
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StartedSession {
    pub id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct TierCounts {
    gold: usize,
    silver: usize,
    bronze: usize,
    edited: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct PieceCounts {
    prose: TierCounts,
    spoken: TierCounts,
    learned: usize,
    total: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct SectionStats {
    pieces: usize,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct VoiceStats {
    schema_version: u32,
    counts: PieceCounts,
    pub sections: BTreeMap<String, SectionStats>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        bridge::{ChecksSummary, FinishReceipt, RulesSummary},
        session::{load_session, valid_session_id},
        test_util::TestDir,
        workspace_tree::{self, scan_sessions},
    };
    use serde_json::Value;
    use std::{collections::HashMap, fs};

    // The editor never reads these three files directly. Deserialize the
    // shared fixtures here to catch structural drift in the persisted inputs
    // that Python turns into the editor's receipt and hygiene views.
    #[allow(dead_code)]
    #[derive(Deserialize)]
    struct DecisionState {
        kind: String,
        version: u32,
        confirmed: BTreeMap<String, DecisionRecord>,
        rejected: BTreeMap<String, DecisionRecord>,
        #[serde(default)]
        retired: BTreeMap<String, DecisionRecord>,
        #[serde(default)]
        scope: BTreeMap<String, String>,
    }

    #[allow(dead_code)]
    #[derive(Deserialize)]
    struct DecisionRecord {
        at: Option<String>,
        section: Option<String>,
    }

    #[allow(dead_code)]
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct EditMeta {
        before_file: String,
        after_file: String,
        before_sha256: Option<String>,
        after_sha256: Option<String>,
        origin: Option<String>,
        section: Option<String>,
        edit_rate: Option<f64>,
    }

    fn records() -> BTreeMap<String, Value> {
        #[derive(Deserialize)]
        struct Golden {
            records: BTreeMap<String, Value>,
        }
        serde_json::from_str::<Golden>(include_str!("../../fixtures/contracts/valid.json"))
            .unwrap()
            .records
    }

    #[test]
    fn reads_python_golden_contracts() {
        let records = records();
        let manifest: Manifest = serde_json::from_value(records["Manifest"].clone()).unwrap();
        assert_eq!(manifest.placeholder, None);
        serde_json::from_value::<VoiceStats>(records["CompiledStats"].clone()).unwrap();
        let rules: RulesSummary = serde_json::from_value(records["RuleSummary"].clone()).unwrap();
        assert!(rules.confirmed.iter().all(|rule| rule.scope.is_none()));
        let checks: ChecksSummary =
            serde_json::from_value(records["ChecksSummary"].clone()).unwrap();
        assert_eq!(checks.pending.len(), 1);
        serde_json::from_value::<FinishReceipt>(records["SessionReceipt"].clone()).unwrap();
        let rules: DecisionState = serde_json::from_value(records["RulesState"].clone()).unwrap();
        assert_eq!(rules.kind, "ghostless-rules");
        assert_eq!(rules.version, 1);
        assert!(rules.confirmed.contains_key("cut:delve"));
        assert!(rules.retired.contains_key("cut:delve"));
        assert!(rules.rejected.contains_key("cut:delve"));
        let checks: DecisionState = serde_json::from_value(records["ChecksState"].clone()).unwrap();
        assert_eq!(checks.kind, "ghostless-checks");
        let check_id = checks.confirmed.keys().next().unwrap();
        assert!(checks.rejected.contains_key(check_id));
        let meta: EditMeta = serde_json::from_value(records["EditMeta"].clone()).unwrap();
        assert_eq!(meta.before_file, "before.md");
        assert_eq!(meta.after_file, "after.md");
    }

    #[test]
    fn ignores_open_status_when_a_receipt_exists() {
        let records = records();
        let project = TestDir::new("contract-session");
        let metadata: SessionMetadata =
            serde_json::from_value(records["SessionMeta"].clone()).unwrap();
        assert!(matches!(metadata.status, Some(SessionStatus::Open)));
        let id = metadata.id.unwrap();
        let folder = project.join("sessions").join(&id);
        fs::create_dir_all(&folder).unwrap();
        for (file, contract) in [
            ("session.json", "SessionMeta"),
            ("receipt.json", "SessionReceipt"),
        ] {
            fs::write(folder.join(file), records[contract].to_string()).unwrap();
        }
        fs::write(
            folder.join("before.md"),
            "Level 9: starts the note. Then we stop.\n",
        )
        .unwrap();
        fs::write(
            folder.join("after.md"),
            "The correction starts the note. Then we stop.\n",
        )
        .unwrap();
        let session = load_session(project.to_path_buf(), id).unwrap();
        assert!(session.finished);
        let sessions = scan_sessions(&project, &HashMap::new());
        assert_eq!(sessions[0].status, workspace_tree::SessionStatus::Finished);
    }

    #[test]
    fn malformed_fields_fail_typed_deserialization() {
        #[derive(Deserialize)]
        struct Invalid {
            schema: String,
            payload: Value,
        }
        let payloads: Vec<Invalid> =
            serde_json::from_str(include_str!("../../fixtures/contracts/invalid.json")).unwrap();
        let mut checked = 0;
        for Invalid { schema, payload } in payloads {
            let rejected = match schema.as_str() {
                "Manifest" => serde_json::from_value::<Manifest>(payload).is_err(),
                "CompiledStats" => serde_json::from_value::<VoiceStats>(payload).is_err(),
                "SessionMeta" => serde_json::from_value::<SessionMetadata>(payload).is_err(),
                "SessionReceipt" => serde_json::from_value::<FinishReceipt>(payload).is_err(),
                "RulesState" | "ChecksState" => {
                    serde_json::from_value::<DecisionState>(payload).is_err()
                }
                "EditMeta" => serde_json::from_value::<EditMeta>(payload).is_err(),
                _ => continue,
            };
            assert!(rejected, "{schema}");
            checked += 1;
        }
        assert_eq!(checked, 7);
    }

    #[test]
    fn shared_session_id_vectors_match() {
        #[derive(Deserialize)]
        struct SessionIdCase {
            id: String,
            valid: bool,
        }
        let cases: Vec<SessionIdCase> =
            serde_json::from_str(include_str!("../../fixtures/contracts/session-ids.json"))
                .unwrap();
        for case in cases {
            assert_eq!(valid_session_id(&case.id), case.valid, "{:?}", case.id);
        }
    }
}
