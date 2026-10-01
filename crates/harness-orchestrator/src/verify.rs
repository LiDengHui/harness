//! Deciding whether a node's result is acceptable.
//!
//! Two independent signals are combined. The deterministic one runs configured
//! commands in the node's worktree and parses their output into structured
//! diagnostics; the adversarial one asks a model to look for what the first
//! signal cannot see, in a fresh tool-free request that never sees the worker's
//! conversation. That separation is the point: a reviewer sharing the executor's
//! conversation would mostly confirm what the executor already believed.
//!
//! `valid` is true only when at least one of the two signals ran and neither
//! objected; `checked` records whether any ran at all. A report with neither
//! checks nor an adversary is `valid == false, checked == false` — nothing was
//! checked, which is a different claim from "checked and passed" and must not be
//! read as one. The commands come from the node when it declares any — a
//! workflow stage's `verify` is the contract for that stage — and from
//! [`VerifierConfig::checks`] otherwise.

use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use harness_core::{HarnessError, Message, Result};
use harness_llm::{ChatRequest, Provider};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::graph::TaskNode;
use crate::result::{json_candidates, SubtaskResult};

/// How long a single deterministic check may run.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

/// Most parsed diagnostics quoted back as evidence.
const MAX_EVIDENCE_LINES: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Issue {
    pub severity: Severity,
    pub summary: String,
    pub evidence: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckOutcome {
    pub name: String,
    pub passed: bool,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerificationReport {
    /// True only when something ran and nothing failed. It is never true for a
    /// report that checked nothing: an empty check list is not a pass.
    pub valid: bool,
    /// True when a deterministic check or the adversarial reviewer actually
    /// ran. `valid == false, checked == false` means "nothing was checked",
    /// where `valid == false, checked == true` means "checked and failed".
    #[serde(default)]
    pub checked: bool,
    pub checks: Vec<CheckOutcome>,
    pub issues: Vec<Issue>,
}

pub struct VerifierConfig {
    /// Commands run in the worktree, e.g. ["cargo", "check"].
    pub checks: Vec<Vec<String>>,
    pub timeout: Duration,
    /// A provider for the adversarial review. Ideally one independent of the
    /// executor's; the CLI wires the run's own, where the separation is the
    /// fresh tool-free request rather than a different model.
    pub adversarial_provider: Option<Arc<dyn Provider>>,
    pub adversarial_model: Option<String>,
}

impl Default for VerifierConfig {
    fn default() -> Self {
        Self {
            checks: Vec::new(),
            timeout: DEFAULT_TIMEOUT,
            adversarial_provider: None,
            adversarial_model: None,
        }
    }
}

pub struct Verifier {
    config: VerifierConfig,
}

impl Verifier {
    pub fn new(config: VerifierConfig) -> Self {
        Self { config }
    }

    pub async fn verify(
        &self,
        node: &TaskNode,
        result: &SubtaskResult,
        worktree: &Path,
    ) -> Result<VerificationReport> {
        let mut checks = Vec::new();
        let mut issues = Vec::new();

        // A node's own commands are the run's contract: a workflow stage
        // declares what "done" means for its stage, and that outranks the
        // executor-wide default. Only a node that declares none falls back to
        // the verifier's configured checks.
        let commands = if node.verify.is_empty() {
            &self.config.checks
        } else {
            &node.verify
        };

        if commands.is_empty() {
            issues.push(Issue {
                severity: Severity::Warning,
                summary: "no deterministic check ran".into(),
                evidence: "neither the node nor the verifier declares a check, so nothing was \
                           compiled, run or tested against this result"
                    .into(),
            });
        } else {
            for command in commands {
                let (outcome, issue) = run_check(command, worktree, self.config.timeout).await;
                if let Some(issue) = issue {
                    issues.push(issue);
                }
                checks.push(outcome);
            }
        }

        let mut adversarial_valid = true;
        let mut adversarial_ran = false;
        match &self.config.adversarial_provider {
            Some(provider) => {
                adversarial_ran = true;
                let model = self
                    .config
                    .adversarial_model
                    .clone()
                    .unwrap_or_else(|| provider.model().to_string());
                let (valid, mut adversarial_issues) =
                    adversarial_review(provider, &model, node, result, &checks).await;
                adversarial_valid = valid;
                issues.append(&mut adversarial_issues);
            }
            None => issues.push(Issue {
                severity: Severity::Info,
                summary: "no adversarial review was run".into(),
                evidence: "no adversarial provider is configured".into(),
            }),
        }

        // A check that could not start still counts as run: it produced a
        // verdict, and that verdict was failure.
        let checked = !checks.is_empty() || adversarial_ran;
        let checks_ok = checks.iter().all(|check| check.passed);
        Ok(VerificationReport {
            valid: checked && checks_ok && adversarial_valid,
            checked,
            checks,
            issues,
        })
    }
}

async fn run_check(
    command: &[String],
    worktree: &Path,
    timeout: Duration,
) -> (CheckOutcome, Option<Issue>) {
    let name = command.join(" ");

    let Some((program, args)) = command.split_first() else {
        return (
            CheckOutcome {
                name,
                passed: false,
                detail: "the command is empty".into(),
            },
            Some(Issue {
                severity: Severity::Error,
                summary: "a configured check is empty".into(),
                evidence: "a check entry has no program to run".into(),
            }),
        );
    };

    let mut process = tokio::process::Command::new(program);
    process
        .args(args)
        .current_dir(worktree)
        .stdin(Stdio::null())
        // Without this a timed-out check keeps running and outlives the run.
        .kill_on_drop(true);

    match tokio::time::timeout(timeout, process.output()).await {
        Err(_) => (
            CheckOutcome {
                name: name.clone(),
                passed: false,
                detail: format!("timed out after {}s", timeout.as_secs()),
            },
            Some(Issue {
                severity: Severity::Error,
                summary: format!("`{name}` timed out"),
                evidence: format!("no exit within {}s", timeout.as_secs()),
            }),
        ),
        Ok(Err(err)) => (
            CheckOutcome {
                name: name.clone(),
                passed: false,
                detail: format!("could not be started: {err}"),
            },
            Some(Issue {
                severity: Severity::Error,
                summary: format!("`{name}` could not be started"),
                evidence: err.to_string(),
            }),
        ),
        Ok(Ok(output)) => {
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let passed = output.status.success();
            let (errors, warnings) = count_diagnostics(&text);
            let tests_failed = test_result_failed(&text);

            let mut detail = match output.status.code() {
                Some(code) => format!("exit {code}; {errors} error(s), {warnings} warning(s)"),
                None => format!("killed; {errors} error(s), {warnings} warning(s)"),
            };
            if tests_failed {
                detail.push_str("; test result: FAILED");
            }

            let issue = (!passed).then(|| Issue {
                severity: Severity::Error,
                summary: format!("`{name}` failed"),
                evidence: evidence_of(&text),
            });

            (
                CheckOutcome {
                    name,
                    passed,
                    detail,
                },
                issue,
            )
        }
    }
}

/// `error[E…]`, `error:` and `warning:` lines, plus a failing test summary.
fn diagnostics(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|line| {
            line.starts_with("error[") || line.starts_with("error:") || line.starts_with("warning:")
        })
        .map(str::to_string)
        .collect();

    if let Some(summary) = text
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("test result:") && line.contains("FAILED"))
    {
        lines.push(summary.to_string());
    }

    lines.truncate(MAX_EVIDENCE_LINES);
    lines
}

fn count_diagnostics(text: &str) -> (usize, usize) {
    let mut errors = 0usize;
    let mut warnings = 0usize;
    for line in text.lines().map(str::trim) {
        if line.starts_with("error[") || line.starts_with("error:") {
            errors += 1;
        } else if line.starts_with("warning:") {
            warnings += 1;
        }
    }
    (errors, warnings)
}

fn test_result_failed(text: &str) -> bool {
    text.lines()
        .map(str::trim)
        .any(|line| line.starts_with("test result:") && line.contains("FAILED"))
}

/// The parsed diagnostics, or the first non-empty output line when there are none.
fn evidence_of(text: &str) -> String {
    let parsed = diagnostics(text);
    if !parsed.is_empty() {
        return parsed.join("\n");
    }
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("the command produced no output")
        .to_string()
}

/// Asks the adversarial reviewer for `{"valid": bool, "issues": [...]}`.
///
/// A reply that cannot be read is treated as a refusal to confirm: claiming a
/// result valid on the strength of an unreadable review is the exact
/// self-confirmation the separate instance exists to avoid.
async fn adversarial_review(
    provider: &Arc<dyn Provider>,
    model: &str,
    node: &TaskNode,
    result: &SubtaskResult,
    checks: &[CheckOutcome],
) -> (bool, Vec<Issue>) {
    let request = ChatRequest::new(
        model.to_string(),
        vec![
            Message::system(ADVERSARIAL_SYSTEM),
            Message::user(adversarial_prompt(node, result, checks)),
        ],
    );

    // The sink must outlive the call; a provider that cannot emit its deltas
    // reports a closed channel as a failure.
    let (events, _rx) = mpsc::unbounded_channel();
    let reply = match provider.stream(request, events).await {
        Ok(response) => response.message.text().to_string(),
        Err(err) => {
            return (
                false,
                vec![Issue {
                    severity: Severity::Error,
                    summary: "the adversarial reviewer could not be reached".into(),
                    evidence: err.to_string(),
                }],
            )
        }
    };

    match parse_adversarial(&reply) {
        Ok((valid, issues)) => (valid, issues),
        Err(err) => (
            false,
            vec![Issue {
                severity: Severity::Error,
                summary: "the adversarial reviewer's reply could not be read".into(),
                evidence: err.to_string(),
            }],
        ),
    }
}

const ADVERSARIAL_SYSTEM: &str = "\
You are an adversarial reviewer. You are given one subtask's objective, the \
result the worker reported, and the outcome of the deterministic checks that \
ran. Your job is to find what is wrong or missing — not to agree.

Reply with exactly one JSON object and nothing else:

  {\"valid\": true, \"issues\": []}

or

  {\"valid\": false, \"issues\": [{\"severity\": \"error\",
    \"summary\": \"one line\", \"evidence\": \"what you saw\"}]}

`valid` must be false when any issue is an error. Do not invent problems: if the \
work is genuinely sound, say so.";

fn adversarial_prompt(node: &TaskNode, result: &SubtaskResult, checks: &[CheckOutcome]) -> String {
    let result_json = serde_json::to_string_pretty(result)
        .unwrap_or_else(|_| "<the result could not be rendered>".to_string());

    let mut checks_text = String::new();
    if checks.is_empty() {
        checks_text.push_str("(no deterministic check ran)");
    } else {
        for check in checks {
            checks_text.push_str(&format!(
                "- `{}`: {}\n",
                check.name,
                if check.passed {
                    format!("passed ({})", check.detail)
                } else {
                    format!("failed ({})", check.detail)
                }
            ));
        }
    }

    format!(
        "Objective:\n{}\n\nReported result:\n{result_json}\n\nDeterministic checks:\n{checks_text}",
        node.objective
    )
}

fn parse_adversarial(reply: &str) -> Result<(bool, Vec<Issue>)> {
    #[derive(Deserialize)]
    struct Reply {
        valid: bool,
        #[serde(default)]
        issues: Vec<RawIssue>,
    }

    #[derive(Deserialize)]
    struct RawIssue {
        #[serde(default)]
        severity: Option<String>,
        #[serde(default)]
        summary: String,
        #[serde(default)]
        evidence: String,
    }

    let mut first_error: Option<String> = None;
    for candidate in json_candidates(reply) {
        let candidate = candidate.trim();
        if !candidate.starts_with('{') {
            continue;
        }
        match serde_json::from_str::<Reply>(candidate) {
            Ok(verdict) => {
                let issues = verdict
                    .issues
                    .into_iter()
                    .map(|raw| Issue {
                        severity: match raw.severity.as_deref() {
                            Some("info") => Severity::Info,
                            Some("warning") => Severity::Warning,
                            _ => Severity::Error,
                        },
                        summary: if raw.summary.trim().is_empty() {
                            "the adversarial reviewer flagged an issue".to_string()
                        } else {
                            raw.summary
                        },
                        evidence: raw.evidence,
                    })
                    .collect();
                return Ok((verdict.valid, issues));
            }
            Err(err) => {
                if first_error.is_none() {
                    first_error = Some(err.to_string());
                }
            }
        }
    }

    Err(HarnessError::Other(first_error.unwrap_or_else(|| {
        "the reply contains no JSON verdict".into()
    })))
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use harness_core::TokenUsage;
    use harness_llm::{ProviderEvent, ProviderResponse};

    use super::*;

    fn node() -> TaskNode {
        TaskNode {
            id: "add-parser".into(),
            objective: "add a parser".into(),
            agent: None,
            depends_on: Vec::new(),
            files: Vec::new(),
            verify: Vec::new(),
        }
    }

    fn result() -> SubtaskResult {
        SubtaskResult {
            objective: "add a parser".into(),
            state: "done".into(),
            evidence: "cargo test passed".into(),
            boundary: "no docs".into(),
        }
    }

    struct ScriptedProvider {
        replies: Mutex<VecDeque<String>>,
        seen: Mutex<Vec<ChatRequest>>,
    }

    impl ScriptedProvider {
        fn new(replies: Vec<&str>) -> Self {
            Self {
                replies: Mutex::new(replies.into_iter().map(str::to_string).collect()),
                seen: Mutex::new(Vec::new()),
            }
        }

        fn requests(&self) -> Vec<ChatRequest> {
            self.seen.lock().expect("lock").clone()
        }
    }

    #[async_trait]
    impl Provider for ScriptedProvider {
        fn id(&self) -> &str {
            "scripted"
        }

        fn model(&self) -> &str {
            "scripted-1"
        }

        async fn stream(
            &self,
            request: ChatRequest,
            _events: mpsc::UnboundedSender<ProviderEvent>,
        ) -> Result<ProviderResponse> {
            self.seen.lock().expect("lock").push(request);
            let text = self
                .replies
                .lock()
                .expect("lock")
                .pop_front()
                .unwrap_or_default();
            Ok(ProviderResponse {
                message: Message::assistant(text),
                tool_calls: Vec::new(),
                usage: TokenUsage::new(1, 1),
                finish_reason: Some("stop".into()),
            })
        }
    }

    fn verifier(config: VerifierConfig) -> Verifier {
        Verifier::new(config)
    }

    #[tokio::test]
    async fn a_node_declared_check_runs_and_outranks_the_configured_one() {
        let tmp = tempfile::tempdir().expect("temp dir");
        // The configured check would fail, but the node declares its own: the
        // node's contract is what runs, so the report is valid.
        let config = VerifierConfig {
            checks: vec![vec!["definitely-not-a-program-xyz".into()]],
            ..VerifierConfig::default()
        };
        let mut with_checks = node();
        with_checks.verify = vec![vec!["git".into(), "--version".into()]];

        let report = verifier(config)
            .verify(&with_checks, &result(), tmp.path())
            .await
            .unwrap();

        assert_eq!(report.checks.len(), 1, "{:?}", report.checks);
        assert_eq!(report.checks[0].name, "git --version");
        assert!(report.checks[0].passed);
        assert!(report.valid);
        assert!(report.checked);
    }

    #[tokio::test]
    async fn a_failing_node_declared_check_makes_the_report_invalid() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let mut with_checks = node();
        with_checks.verify = vec![vec![
            "git".into(),
            "rev-parse".into(),
            "--verify".into(),
            "--quiet".into(),
            "HEAD".into(),
        ]];

        let report = verifier(VerifierConfig::default())
            .verify(&with_checks, &result(), tmp.path())
            .await
            .unwrap();

        assert!(!report.valid, "{:?}", report.checks);
        assert!(report.checked);
        assert!(!report.checks[0].passed);
    }

    #[tokio::test]
    async fn an_empty_check_list_is_reported_rather_than_passing_silently() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let report = verifier(VerifierConfig::default())
            .verify(&node(), &result(), tmp.path())
            .await
            .unwrap();

        assert!(report.checks.is_empty());
        // Nothing failed because nothing ran. The report must not present that
        // as a pass: `checked` is what tells the two apart.
        assert!(
            !report.valid,
            "nothing was checked, so nothing was verified"
        );
        assert!(!report.checked);
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.summary.contains("no deterministic check ran")),
            "{:?}",
            report.issues
        );
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.summary.contains("no adversarial review")));
    }

    #[tokio::test]
    async fn a_passing_check_is_recorded() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let config = VerifierConfig {
            checks: vec![vec!["git".into(), "--version".into()]],
            ..VerifierConfig::default()
        };
        let report = verifier(config)
            .verify(&node(), &result(), tmp.path())
            .await
            .unwrap();

        assert_eq!(report.checks.len(), 1);
        assert!(report.checks[0].passed);
        assert!(report.valid);
        assert!(report.checked);
    }

    #[tokio::test]
    async fn a_failing_check_produces_an_error_issue() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let config = VerifierConfig {
            checks: vec![vec![
                "git".into(),
                "rev-parse".into(),
                "--verify".into(),
                "--quiet".into(),
                "HEAD".into(),
            ]],
            ..VerifierConfig::default()
        };
        let report = verifier(config)
            .verify(&node(), &result(), tmp.path())
            .await
            .unwrap();

        assert!(!report.valid);
        assert!(report.checked);
        assert!(!report.checks[0].passed);
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.severity == Severity::Error));
    }

    #[tokio::test]
    async fn a_check_that_cannot_start_is_an_error_not_a_panic() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let config = VerifierConfig {
            checks: vec![vec!["definitely-not-a-program-xyz".into()]],
            ..VerifierConfig::default()
        };
        let report = verifier(config)
            .verify(&node(), &result(), tmp.path())
            .await
            .unwrap();
        assert!(!report.valid);
        assert!(report.checked);
        assert!(!report.checks[0].passed);
    }

    #[tokio::test]
    async fn a_hung_check_times_out_instead_of_hanging_the_run() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let command = if cfg!(windows) {
            vec![
                "cmd".to_string(),
                "/C".to_string(),
                "ping".to_string(),
                "-n".to_string(),
                "6".to_string(),
                "127.0.0.1".to_string(),
            ]
        } else {
            vec!["sleep".to_string(), "5".to_string()]
        };
        let config = VerifierConfig {
            checks: vec![command],
            timeout: Duration::from_millis(300),
            ..VerifierConfig::default()
        };

        let started = std::time::Instant::now();
        let report = verifier(config)
            .verify(&node(), &result(), tmp.path())
            .await
            .unwrap();

        assert!(!report.valid);
        assert!(report.checked);
        assert!(report.checks[0].detail.contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(4));
    }

    #[tokio::test]
    async fn an_adversarial_rejection_makes_the_report_invalid() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let provider: Arc<dyn Provider> = Arc::new(ScriptedProvider::new(vec![
            r#"{"valid": false, "issues": [{"severity": "error", "summary": "no test covers the new branch", "evidence": "src/parse.rs has no #[test]"}]}"#,
        ]));
        let config = VerifierConfig {
            adversarial_provider: Some(provider),
            adversarial_model: Some("scripted-1".into()),
            ..VerifierConfig::default()
        };

        let report = verifier(config)
            .verify(&node(), &result(), tmp.path())
            .await
            .unwrap();

        assert!(!report.valid);
        assert!(report.checked);
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.summary.contains("no test covers")));
    }

    #[tokio::test]
    async fn the_adversarial_reviewer_sees_only_the_evidence_it_needs() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let scripted = Arc::new(ScriptedProvider::new(vec![
            r#"{"valid": true, "issues": []}"#,
        ]));
        let provider: Arc<dyn Provider> = scripted.clone();
        let config = VerifierConfig {
            checks: vec![vec!["git".into(), "--version".into()]],
            adversarial_provider: Some(provider),
            adversarial_model: Some("scripted-1".into()),
            ..VerifierConfig::default()
        };

        let report = verifier(config)
            .verify(&node(), &result(), tmp.path())
            .await
            .unwrap();
        assert!(report.valid);
        assert!(report.checked);

        let requests = scripted.requests();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].tools.is_empty());
        let user = requests[0]
            .messages
            .iter()
            .find(|message| message.role == harness_core::Role::User)
            .expect("a user message");
        assert!(user.text().contains("add a parser"), "{}", user.text());
        assert!(user.text().contains("cargo test passed"), "{}", user.text());
        assert!(user.text().contains("git --version"), "{}", user.text());
    }

    #[tokio::test]
    async fn an_unreadable_adversarial_reply_is_not_taken_as_approval() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let provider: Arc<dyn Provider> = Arc::new(ScriptedProvider::new(vec!["looks fine to me"]));
        let config = VerifierConfig {
            adversarial_provider: Some(provider),
            adversarial_model: Some("scripted-1".into()),
            ..VerifierConfig::default()
        };

        let report = verifier(config)
            .verify(&node(), &result(), tmp.path())
            .await
            .unwrap();
        assert!(!report.valid);
        assert!(report.checked);
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.summary.contains("could not be read")));
    }

    #[tokio::test]
    async fn the_adversarial_model_falls_back_to_the_provider_model() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let scripted = Arc::new(ScriptedProvider::new(vec![r#"{"valid": true}"#]));
        let provider: Arc<dyn Provider> = scripted.clone();
        let config = VerifierConfig {
            adversarial_provider: Some(provider),
            adversarial_model: None,
            ..VerifierConfig::default()
        };

        verifier(config)
            .verify(&node(), &result(), tmp.path())
            .await
            .unwrap();
        assert_eq!(scripted.requests()[0].model, "scripted-1");
    }

    #[tokio::test]
    async fn the_three_verification_states_are_distinct() {
        let tmp = tempfile::tempdir().expect("temp dir");

        let nothing = verifier(VerifierConfig::default())
            .verify(&node(), &result(), tmp.path())
            .await
            .unwrap();
        let passing = verifier(VerifierConfig {
            checks: vec![vec!["git".into(), "--version".into()]],
            ..VerifierConfig::default()
        })
        .verify(&node(), &result(), tmp.path())
        .await
        .unwrap();
        let failing = verifier(VerifierConfig {
            checks: vec![vec![
                "git".into(),
                "rev-parse".into(),
                "--verify".into(),
                "--quiet".into(),
                "HEAD".into(),
            ]],
            ..VerifierConfig::default()
        })
        .verify(&node(), &result(), tmp.path())
        .await
        .unwrap();

        // (valid, checked): nothing ran, ran and passed, ran and failed.
        assert_eq!((nothing.valid, nothing.checked), (false, false));
        assert_eq!((passing.valid, passing.checked), (true, true));
        assert_eq!((failing.valid, failing.checked), (false, true));
    }

    #[test]
    fn rust_diagnostics_are_parsed_not_dumped() {
        let output = "\
   Compiling harness-core v0.1.0
error[E0425]: cannot find value `x` in this scope
 --> src/lib.rs:3:5
warning: unused variable: `y`
error: could not compile `harness-core`

test result: FAILED. 1 passed; 1 failed; 0 ignored";
        let (errors, warnings) = count_diagnostics(output);
        assert_eq!(errors, 2);
        assert_eq!(warnings, 1);
        assert!(test_result_failed(output));

        let evidence = evidence_of(output);
        assert!(evidence.contains("error[E0425]"), "{evidence}");
        assert!(evidence.contains("test result: FAILED"), "{evidence}");
        assert!(!evidence.contains("Compiling"), "{evidence}");
    }

    #[test]
    fn evidence_falls_back_to_the_first_output_line() {
        assert_eq!(
            evidence_of("fatal: not a git repository"),
            "fatal: not a git repository"
        );
        assert_eq!(evidence_of("   \n\n"), "the command produced no output");
    }

    #[test]
    fn a_review_with_no_issues_parses() {
        let (valid, issues) = parse_adversarial(r#"{"valid": true, "issues": []}"#).unwrap();
        assert!(valid);
        assert!(issues.is_empty());
    }

    #[test]
    fn an_unknown_severity_is_treated_as_an_error() {
        let (valid, issues) = parse_adversarial(
            r#"{"valid": false, "issues": [{"severity": "catastrophic", "summary": "bad"}]}"#,
        )
        .unwrap();
        assert!(!valid);
        assert_eq!(issues[0].severity, Severity::Error);
    }
}
