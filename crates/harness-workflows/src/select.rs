//! Choosing the workflow that fits a task.
//!
//! The selector is the cheap half of progressive disclosure. It shows the model
//! a catalog of `id`, `name`, `when` and `description` — never the stages and
//! never the guidance — and asks for one id back. The stages and the guidance
//! are paid for only once a workflow is actually run, exactly as a skill's body
//! is paid for only on activation.
//!
//! Cheap is also enforced structurally: one turn, no tools, a low temperature
//! and a small output cap. Nothing here reads the repository.
//!
//! Honesty is part of the contract. `null` is a correct answer, and the prompt
//! says so in as many words; a model that picks a workflow for an unrelated task
//! costs more than the planning it was meant to avoid.

use harness_core::{HarnessError, Message, Result};
use harness_llm::{ChatRequest, Provider};
use serde::Deserialize;
use tokio::sync::mpsc;

use crate::{WorkflowRegistry, WorkflowSpec};

/// Instructions for the selector. See the module docs for why it is this small.
pub const SELECT_SYSTEM_PROMPT: &str = "\
你是一个工作流选择器。你只做一件事：判断下面这个任务应当用哪一个已有工作流来跑。

规则：
- 只依据每个工作流的 id、名称、适用场景(when) 和描述来判断；你拿不到工作流的内部步骤，也不要臆测它内部会做什么。
- 只有当某个工作流的适用场景与任务真正吻合时才选它。
- 如果没有一个工作流真正适合这个任务，返回 null。这是完全正确的答案，不要为了给出答案而勉强选一个。
- 不确定时返回 null，而不是猜。

只回复一个 JSON 对象，不要有其他文字：

  {\"workflow\": \"<id>\"}

或

  {\"workflow\": null}";

/// Largest reply the selector is allowed to produce: an id and some JSON.
const SELECT_MAX_TOKENS: u32 = 128;

impl WorkflowRegistry {
    /// Picks the workflow that best fits `task`, or `None` when nothing does.
    ///
    /// One tool-free turn against `provider`. The catalog the model sees holds
    /// only metadata, so this stays cheap even with a large registry. A reply
    /// that names an id outside the catalog, or that cannot be read at all, is
    /// an error rather than a guess: `None` means "nothing fits", and only the
    /// model saying so can produce it.
    pub async fn select(
        &self,
        task: &str,
        provider: &dyn Provider,
        model: &str,
    ) -> Result<Option<&WorkflowSpec>> {
        // An empty catalog has no answer to give, so no tokens are spent.
        if self.is_empty() {
            return Ok(None);
        }

        let mut request = ChatRequest::new(
            model.to_string(),
            vec![
                Message::system(SELECT_SYSTEM_PROMPT),
                Message::user(select_prompt(task, self)),
            ],
        );
        request.temperature = Some(0.0);
        request.max_tokens = Some(SELECT_MAX_TOKENS);
        // `tools` is deliberately left empty: the selector must not explore.

        // The sink must outlive the call: a provider that cannot emit its
        // deltas treats a closed channel as a failure.
        let (events, _rx) = mpsc::unbounded_channel();
        let reply = provider.stream(request, events).await?;

        match parse_selection(reply.message.text())? {
            None => Ok(None),
            Some(id) => match self.get(&id) {
                Some(spec) => Ok(Some(spec)),
                None => Err(HarnessError::Other(format!(
                    "the workflow selector chose `{id}`, which is not in the catalog \
                     ({} known: {})",
                    self.len(),
                    self.names().join(", ")
                ))),
            },
        }
    }
}

/// The user turn: the task, then the metadata-only catalog.
fn select_prompt(task: &str, registry: &WorkflowRegistry) -> String {
    let mut catalog = String::new();
    for spec in registry.list() {
        catalog.push_str(&format!(
            "- id: {} | 名称: {} | 适用: {} | 说明: {}\n",
            spec.id, spec.name, spec.when, spec.description
        ));
    }

    format!("任务：\n{}\n\n可选工作流：\n{catalog}", task.trim())
}

/// Reads `{"workflow": "<id>" | null}` out of a reply.
///
/// `Ok(None)` is the explicit "nothing fits" answer. A reply with no JSON
/// object, or with JSON that does not carry the field, is an error: it is not
/// the model declining, it is the selector failing.
fn parse_selection(reply: &str) -> Result<Option<String>> {
    let Some(candidate) = first_json_object(reply) else {
        return Err(HarnessError::Other(format!(
            "the workflow selector returned no JSON object:\n{}",
            reply.trim()
        )));
    };

    #[derive(Deserialize)]
    struct Selection {
        #[serde(default)]
        workflow: Option<String>,
    }

    let selection: Selection = serde_json::from_str(candidate).map_err(|err| {
        HarnessError::Other(format!(
            "the workflow selector's reply could not be read: {err}\n{}",
            reply.trim()
        ))
    })?;

    match selection.workflow.as_deref().map(str::trim) {
        // `"none"`/`"null"` as strings are what models write when they mean the
        // JSON null but are unsure of the schema.
        None | Some("") | Some("none") | Some("null") | Some("无") | Some("不适用") => Ok(None),
        Some(id) => Ok(Some(id.to_string())),
    }
}

/// The first balanced `{...}` in `reply`, ignoring braces inside strings.
///
/// Models wrap JSON in prose or a fenced block often enough that the planner
/// has the same helper; this crate keeps its own because that one is private to
/// the orchestrator, and a dependency on it would be heavier than the twenty
/// lines it saves.
fn first_json_object(reply: &str) -> Option<&str> {
    let start = reply.find('{')?;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for (offset, byte) in reply.as_bytes()[start..].iter().enumerate() {
        if in_string {
            match byte {
                b'\\' if !escaped => escaped = true,
                b'"' if !escaped => in_string = false,
                _ => escaped = false,
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    // `{` and `}` are ASCII, so these offsets are char
                    // boundaries.
                    return Some(&reply[start..start + offset + 1]);
                }
            }
            _ => {}
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use harness_core::TokenUsage;
    use harness_llm::{ProviderEvent, ProviderResponse};

    use super::*;
    use crate::WorkflowStage;

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
                usage: TokenUsage::new(10, 5),
                finish_reason: Some("stop".into()),
            })
        }
    }

    fn spec(id: &str, when: &str) -> WorkflowSpec {
        WorkflowSpec {
            id: id.into(),
            name: format!("{id} 的名称"),
            description: format!("{id} 的描述"),
            when: when.into(),
            stages: vec![WorkflowStage {
                id: "内部阶段".into(),
                objective: "内部目标不应出现在选择提示里".into(),
                agent: Some("default".into()),
                depends_on: Vec::new(),
                verify: vec![vec!["cargo".into(), "test".into()]],
            }],
            guidance: "内部指导不应出现在选择提示里".into(),
            source_path: PathBuf::from("nowhere"),
        }
    }

    fn registry() -> WorkflowRegistry {
        WorkflowRegistry {
            by_id: vec![
                spec("bug-fix", "有可复现的缺陷，目标是把现有行为改对。"),
                spec("feature-implement", "要新增一个当前不存在的功能。"),
            ]
            .into_iter()
            .map(|spec| (spec.id.clone(), spec))
            .collect(),
            known_agents: None,
        }
    }

    #[tokio::test]
    async fn a_clear_match_is_selected() {
        let provider = ScriptedProvider::new(vec![r#"{"workflow": "bug-fix"}"#]);
        let registry = registry();
        let chosen = registry
            .select("修一下这个崩溃", &provider, "scripted-1")
            .await
            .unwrap();

        assert_eq!(chosen.map(|spec| spec.id.as_str()), Some("bug-fix"));
        assert_eq!(provider.requests().len(), 1, "one turn only");
    }

    #[tokio::test]
    async fn nothing_fitting_returns_none() {
        for reply in [
            r#"{"workflow": null}"#,
            r#"{"workflow": "none"}"#,
            r#"{"workflow": ""}"#,
        ] {
            let provider = ScriptedProvider::new(vec![reply]);
            let registry = registry();
            let chosen = registry
                .select("把仓库的配色换成蓝色", &provider, "scripted-1")
                .await
                .unwrap();
            assert!(chosen.is_none(), "reply {reply:?} means nothing fits");
        }
    }

    #[tokio::test]
    async fn malformed_output_is_an_error_rather_than_a_guess() {
        for reply in [
            "我觉得 bug-fix 挺合适的",
            r#"{"workflow": "#,
            "no json here",
        ] {
            let provider = ScriptedProvider::new(vec![reply]);
            let err = registry()
                .select("修一下这个崩溃", &provider, "scripted-1")
                .await
                .unwrap_err();
            assert!(err.to_string().contains("selector"), "{err}");
        }
    }

    #[tokio::test]
    async fn an_id_outside_the_catalog_is_an_error() {
        let provider = ScriptedProvider::new(vec![r#"{"workflow": "ghost"}"#]);
        let err = registry()
            .select("修一下这个崩溃", &provider, "scripted-1")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("ghost"), "{err}");
        assert!(err.to_string().contains("not in the catalog"), "{err}");
    }

    #[tokio::test]
    async fn the_catalog_carries_metadata_only_and_no_tools() {
        let provider = ScriptedProvider::new(vec![r#"{"workflow": "bug-fix"}"#]);
        registry()
            .select("修一下这个崩溃", &provider, "scripted-1")
            .await
            .unwrap();

        let requests = provider.requests();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert!(request.tools.is_empty(), "the selector must not explore");
        assert_eq!(request.temperature, Some(0.0));
        assert_eq!(request.max_tokens, Some(SELECT_MAX_TOKENS));
        assert_eq!(request.model, "scripted-1");

        let user = request
            .messages
            .iter()
            .find(|message| message.role == harness_core::Role::User)
            .expect("a user message");
        let text = user.text();
        assert!(text.contains("修一下这个崩溃"), "{text}");
        assert!(text.contains("bug-fix"), "{text}");
        assert!(
            text.contains("有可复现的缺陷"),
            "the `when` field is the selector's signal: {text}"
        );
        assert!(
            !text.contains("内部目标"),
            "stages must not reach the selector: {text}"
        );
        assert!(
            !text.contains("内部指导"),
            "guidance must not reach the selector: {text}"
        );

        let system = request
            .messages
            .iter()
            .find(|message| message.role == harness_core::Role::System)
            .expect("a system message");
        assert!(system.text().contains("null"), "{}", system.text());
    }

    #[tokio::test]
    async fn an_empty_registry_spends_no_tokens() {
        let provider = ScriptedProvider::new(vec![r#"{"workflow": "anything"}"#]);
        let empty = WorkflowRegistry::default();
        let chosen = empty
            .select("随便什么任务", &provider, "scripted-1")
            .await
            .unwrap();

        assert!(chosen.is_none());
        assert!(
            provider.requests().is_empty(),
            "no catalog means nothing to ask"
        );
    }

    #[test]
    fn the_first_json_object_is_extracted_from_prose() {
        assert_eq!(
            first_json_object("当然：\n```json\n{\"workflow\": \"bug-fix\"}\n```\n"),
            Some(r#"{"workflow": "bug-fix"}"#)
        );
        assert_eq!(
            first_json_object(r#"{"workflow": "a}b"}"#),
            Some(r#"{"workflow": "a}b"}"#),
            "braces inside strings do not close the object"
        );
        assert_eq!(first_json_object("no braces"), None);
    }
}
