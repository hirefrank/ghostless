use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RulesSummary {
    pub minimum_support: usize,
    pub hypotheses: Vec<Rule>,
    pub confirmed: Vec<Rule>,
    pub rejected: Vec<Rule>,
    pub retired: Vec<Rule>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Rule {
    pub id: String,
    #[serde(default)]
    pub support: usize,
    #[serde(default)]
    pub sections: Vec<String>,
    #[serde(default)]
    pub examples: Vec<RuleExample>,
    #[serde(default)]
    pub counterexamples: Vec<serde_json::Value>,
    #[serde(default)]
    pub receipts: Vec<String>,
    pub scope: Option<String>,
    pub confirmed_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct RuleExample {
    pub before_snippet: String,
    pub after_snippet: String,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct ChecksSummary {
    pub pending: Vec<Check>,
    pub confirmed: Vec<Check>,
    pub rejected: Vec<Check>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct Check {
    pub id: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub span: String,
    #[serde(default)]
    pub polarity: String,
    #[serde(default)]
    pub pair: String,
    pub section: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{cli::run_cli, test_util::TestDir};
    use serde_json::{Value, json};

    fn method(project: &str, name: &str) -> Value {
        let payload = json!({"id": 1, "method": name, "params": {"project": project}}).to_string();
        let output = run_cli(&["bridge"], Some(&format!("{payload}\n"))).unwrap();
        let envelope: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(envelope["ok"], true, "{envelope}");
        envelope["result"].clone()
    }

    #[test]
    fn deserialize_real_rules_and_pending_checks() {
        let root = TestDir::new("voice-review-json");
        let project = root.to_str().unwrap();
        run_cli(&["init", "--no-git", project], None).unwrap();
        std::fs::write(
            root.join("rules.json"),
            r#"{"kind":"ghostless-rules","version":1,"confirmed":{"cut:delve":{"at":"2026-01-02T00:00:00Z","receipts":["gold/first"]}},"rejected":{},"retired":{},"scope":{"cut:delve":"notes"}}"#,
        ).unwrap();
        let before = root.join("a.md");
        let after = root.join("b.md");
        std::fs::write(&before, "Level 9: The preview opened with a long title.\n").unwrap();
        std::fs::write(&after, "The correction came first.\n").unwrap();
        run_cli(
            &[
                "learn",
                "--project",
                project,
                "--before",
                before.to_str().unwrap(),
                "--after",
                after.to_str().unwrap(),
                "--section",
                "notes",
                "--origin",
                "writer-first",
                "--check",
                r#"The opener avoids "Level 9:"."#,
            ],
            None,
        )
        .unwrap();
        let rules: RulesSummary = serde_json::from_value(method(project, "rules_list")).unwrap();
        let checks: ChecksSummary = serde_json::from_value(method(project, "checks_list")).unwrap();
        assert_eq!(rules.minimum_support, 3);
        assert_eq!(rules.confirmed[0].id, "cut:delve");
        assert_eq!(rules.confirmed[0].scope.as_deref(), Some("notes"));
        assert_eq!(rules.confirmed[0].receipts, ["gold/first"]);
        assert_eq!(checks.pending[0].span, "Level 9:");
        assert_eq!(checks.pending[0].polarity, "forbid");
        assert_eq!(checks.pending[0].section.as_deref(), Some("notes"));
    }
}
