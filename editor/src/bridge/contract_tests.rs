use super::*;

#[derive(Deserialize)]
struct Exchange {
    request: Request,
    response: Value,
}

#[derive(Deserialize)]
struct Request {
    id: u64,
    #[serde(flatten)]
    op: BridgeOp,
}

#[test]
fn python_bridge_goldens_match_editor_requests_and_replies() {
    let fixture: Value =
        serde_json::from_str(include_str!("../../../fixtures/contracts/valid.json")).unwrap();
    let expected = [
        "judge_text",
        "build",
        "sessions_list",
        "session_start",
        "session_finish",
        "promote",
        "checks_list",
        "checks_decide",
        "rules_list",
        "rules_decide",
        "save_note",
    ];
    let mut seen = std::collections::BTreeSet::new();
    for exchange in fixture["bridge"].as_array().unwrap() {
        let method = exchange["request"]["method"].as_str().unwrap();
        if !expected.contains(&method) {
            validate_cli_only_result(method, &exchange["response"]);
            continue;
        }
        seen.insert(method);
        let Exchange { request, response } = serde_json::from_value(exchange.clone())
            .unwrap_or_else(|err| panic!("{method} request: {err}"));
        let job = Job {
            id: request.id,
            op: request.op,
            doc_id: None,
            generation: 0,
            section_label: String::new(),
        };
        let sent: Value = serde_json::from_str(&job.payload().unwrap()).unwrap();
        for (key, value) in exchange["request"]["params"].as_object().unwrap() {
            assert_eq!(&sent["params"][key], value, "{method}.{key}");
        }
        let reply = job.decode(&response.to_string());
        let accepted = match &reply {
            BridgeReply::Judge { result, .. } => {
                let expected = &response["result"];
                assert_eq!(
                    result.error.as_deref(),
                    expected["error"].as_str(),
                    "{reply:?}"
                );
                assert_eq!(
                    result.score as i64,
                    expected["score"].as_i64().unwrap_or_default()
                );
                true
            }
            BridgeReply::Build { result, .. } => result.is_ok(),
            BridgeReply::SessionsList { result } => result.is_ok(),
            BridgeReply::SessionStart { result, .. } => result.is_ok(),
            BridgeReply::SessionFinish { result, .. } => {
                if let Ok(receipt) = result {
                    let expected = response["result"]["warnings"].as_array();
                    assert_eq!(receipt.warnings.len(), expected.map_or(0, Vec::len));
                }
                result.is_ok()
            }
            BridgeReply::Promote { result } => result.is_ok(),
            BridgeReply::ChecksList { result, .. } => result.as_ref().is_ok_and(|checks| {
                checks.rejected.iter().all(|rejected| {
                    checks
                        .pending
                        .iter()
                        .chain(&checks.confirmed)
                        .all(|check| check.id != rejected.id)
                })
            }),
            BridgeReply::ChecksDecide { result, .. } => result.is_ok(),
            BridgeReply::RulesList { result, .. } => result.as_ref().is_ok_and(|rules| {
                rules.rejected.iter().all(|rejected| {
                    rules
                        .retired
                        .iter()
                        .chain(&rules.confirmed)
                        .chain(&rules.hypotheses)
                        .all(|rule| rule.id != rejected.id)
                })
            }),
            BridgeReply::RulesDecide { result, .. } => result.is_ok(),
            BridgeReply::SaveNote { result } => result.is_ok(),
        };
        assert_eq!(accepted, response["ok"] == true, "{method}: {reply:?}");
    }
    assert_eq!(seen.len(), expected.len());
}

fn validate_cli_only_result(method: &str, response: &Value) {
    // The Python CLI implements these methods and the editor has no callers.
    #[derive(Deserialize)]
    struct Ping {
        version: String,
    }
    #[derive(Deserialize)]
    struct Resolve {
        project: Option<PathBuf>,
        source: Option<String>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Snapshot {
        id: String,
        path: String,
        saved_at: String,
    }

    let body = &response["result"];
    assert_eq!(response["ok"], true);
    match method {
        "ping" => {
            let result: Ping = serde_json::from_value(body.clone()).unwrap();
            assert!(!result.version.is_empty());
        }
        "resolve_project" => {
            let result: Resolve = serde_json::from_value(body.clone()).unwrap();
            assert!(result.project.is_some() && result.source.is_some());
        }
        "session_snapshot" => {
            let result: Snapshot = serde_json::from_value(body.clone()).unwrap();
            assert!(crate::session::valid_session_id(&result.id));
            assert_eq!(result.path, format!("sessions/{}", result.id));
            assert!(!result.saved_at.is_empty());
        }
        _ => panic!("Unexpected fixture method {method}"),
    }
}

#[test]
fn bridge_fixture_rejects_malformed_contracts() {
    #[derive(Deserialize)]
    struct Invalid {
        schema: String,
        payload: Value,
    }
    let invalid: Vec<Invalid> =
        serde_json::from_str(include_str!("../../../fixtures/contracts/invalid.json")).unwrap();
    let mut checked = 0;
    for Invalid { schema, payload } in invalid {
        match schema.as_str() {
            "BridgeRequest" => {
                let request: Request = serde_json::from_value(payload).unwrap();
                let job = Job {
                    id: request.id,
                    op: request.op,
                    doc_id: None,
                    generation: 0,
                    section_label: String::new(),
                };
                assert!(job.payload().is_err());
            }
            "BridgeResponse" => {
                let decoded: Envelope = serde_json::from_value(payload).unwrap();
                assert!(decoded.into_payload(3).is_err());
            }
            "judge_textResult" => {
                assert!(serde_json::from_value::<JudgeReport>(payload).is_err());
            }
            _ => continue,
        }
        checked += 1;
    }
    assert_eq!(checked, 4);
}
