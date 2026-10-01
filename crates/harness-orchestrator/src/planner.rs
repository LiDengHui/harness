//! Turning one task into a task DAG.
//!
//! The planner runs a light model in a single, tool-free turn. It is given no
//! tools on purpose: the token policy for a planning call assumes a small
//! request, and a planner that could read the repository would spend the run's
//! budget exploring instead of deciding. If the reply cannot be read as a DAG,
//! the parse error is handed straight back to the model once before the planner
//! gives up.

use std::sync::Arc;

use harness_core::{HarnessError, Message, Result};
use harness_llm::{ChatRequest, Provider};
use tokio::sync::mpsc;

use crate::graph::{TaskGraph, TaskNode};
use crate::result::json_candidates;

/// What the planner is asked to produce.
const SYSTEM_PROMPT: &str = "\
You are a planning agent. You turn one task into a directed acyclic graph of \
independent subtasks. You never write code and you never run tools; you only \
decide the work and its order.

Reply with exactly one JSON object and nothing else:

  {\"nodes\": [{\"id\": \"slug\", \"objective\": \"...\", \"agent\": \"agent-id\",
              \"depends_on\": [\"other-slug\"], \"files\": [\"path/claimed.rs\"]}]}

Rules:
- `id` is a short kebab-case slug, unique in the graph.
- `objective` says what the node must achieve, and what \"done\" means.
- `depends_on` lists ids of nodes that must finish first. It must be acyclic and
  every id must exist.
- `files` lists the files the node claims to touch, so two nodes in the same
  parallel layer can be shown to conflict before either runs.
- `agent` is optional; omit it to use the default agent.
- Prefer a few substantial nodes over many tiny ones. Never emit a node with an
  empty objective.";

pub struct Planner {
    provider: Arc<dyn Provider>,
    model: String,
    max_nodes: usize,
}

impl Planner {
    pub fn new(provider: Arc<dyn Provider>, model: impl Into<String>, max_nodes: usize) -> Self {
        Self {
            provider,
            model: model.into(),
            max_nodes,
        }
    }

    /// Asks the model for a DAG. On a malformed reply it retries ONCE, feeding
    /// the parse error back, then fails with the model's own output quoted.
    pub async fn plan(&self, task: &str, context: Option<&str>) -> Result<TaskGraph> {
        let first = self.ask(task, context, None).await?;
        let first_error = match self.read_graph(&first) {
            Ok(graph) => return Ok(graph),
            Err(err) => err,
        };

        let retry_prompt = format!(
            "The previous reply was rejected: {first_error}\n\nReply again with a \
             corrected JSON object only."
        );
        let second = self.ask(task, context, Some(&retry_prompt)).await?;

        match self.read_graph(&second) {
            Ok(graph) => Ok(graph),
            Err(second_error) => Err(HarnessError::Other(format!(
                "the planner returned no usable task graph after one retry: {second_error}\n\
                 --- model reply ---\n{second}"
            ))),
        }
    }

    async fn ask(
        &self,
        task: &str,
        context: Option<&str>,
        correction: Option<&str>,
    ) -> Result<String> {
        let mut prompt = format!("Task:\n{task}");
        if let Some(context) = context {
            if !context.trim().is_empty() {
                prompt.push_str(&format!("\n\nContext:\n{context}"));
            }
        }
        if let Some(correction) = correction {
            prompt.push_str(&format!("\n\n{correction}"));
        }

        // No tools are attached: `ChatRequest::new` leaves `tools` empty and it
        // is deliberately never filled in here.
        let request = ChatRequest::new(
            self.model.clone(),
            vec![Message::system(SYSTEM_PROMPT), Message::user(prompt)],
        );

        // The sink must outlive the call: a provider that cannot emit its
        // deltas treats a closed channel as a failure.
        let (events, _rx) = mpsc::unbounded_channel();
        let response = self.provider.stream(request, events).await?;
        Ok(response.message.text().to_string())
    }

    fn read_graph(&self, reply: &str) -> Result<TaskGraph> {
        let graph = parse_graph(reply)?;

        if self.max_nodes > 0 && graph.nodes.len() > self.max_nodes {
            return Err(HarnessError::Other(format!(
                "the plan has {} nodes, more than the limit of {}",
                graph.nodes.len(),
                self.max_nodes
            )));
        }

        graph.validate()?;
        Ok(graph)
    }
}

fn parse_graph(reply: &str) -> Result<TaskGraph> {
    let mut first_error: Option<HarnessError> = None;

    for candidate in json_candidates(reply) {
        let candidate = candidate.trim();
        let parsed = if candidate.starts_with('{') {
            serde_json::from_str::<TaskGraph>(candidate).map_err(|err| err.to_string())
        } else if candidate.starts_with('[') {
            // Models frequently answer with a bare array of nodes.
            serde_json::from_str::<Vec<TaskNode>>(candidate)
                .map(|nodes| TaskGraph {
                    nodes,
                    guidance: None,
                })
                .map_err(|err| err.to_string())
        } else {
            continue;
        };

        match parsed {
            Ok(graph) => return Ok(graph),
            Err(message) => {
                if first_error.is_none() {
                    first_error = Some(HarnessError::Other(format!(
                        "the reply is not a task graph: {message}"
                    )));
                }
            }
        }
    }

    Err(first_error
        .unwrap_or_else(|| HarnessError::Other("the reply contains no JSON task graph".into())))
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use harness_core::TokenUsage;
    use harness_llm::{ProviderEvent, ProviderResponse};

    use super::*;

    struct RecordingProvider {
        replies: Mutex<VecDeque<String>>,
        seen: Mutex<Vec<ChatRequest>>,
    }

    impl RecordingProvider {
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
    impl Provider for RecordingProvider {
        fn id(&self) -> &str {
            "recording"
        }

        fn model(&self) -> &str {
            "recording-1"
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

    const GOOD: &str = r#"{"nodes":[
        {"id":"a","objective":"write the parser","files":["src/parse.rs"]},
        {"id":"b","objective":"wire the parser in","depends_on":["a"]}
    ]}"#;

    fn planner(provider: Arc<RecordingProvider>, max_nodes: usize) -> Planner {
        Planner::new(provider, "recording-1", max_nodes)
    }

    #[tokio::test]
    async fn a_good_reply_becomes_a_graph() {
        let provider = Arc::new(RecordingProvider::new(vec![GOOD]));
        let graph = planner(provider, 8)
            .plan("build a parser", None)
            .await
            .unwrap();

        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(graph.get("b").unwrap().depends_on, vec!["a".to_string()]);
    }

    #[tokio::test]
    async fn the_planner_sends_no_tools() {
        let provider = Arc::new(RecordingProvider::new(vec![GOOD]));
        planner(Arc::clone(&provider), 8)
            .plan("build a parser", None)
            .await
            .unwrap();

        let requests = provider.requests();
        assert_eq!(requests.len(), 1);
        assert!(
            requests[0].tools.is_empty(),
            "the planner must never carry tools"
        );
    }

    #[tokio::test]
    async fn the_task_and_context_reach_the_model() {
        let provider = Arc::new(RecordingProvider::new(vec![GOOD]));
        planner(Arc::clone(&provider), 8)
            .plan("build a parser", Some("the repo already has a lexer"))
            .await
            .unwrap();

        let requests = provider.requests();
        let user = requests[0]
            .messages
            .iter()
            .find(|message| message.role == harness_core::Role::User)
            .unwrap();
        assert!(user.text().contains("build a parser"), "{}", user.text());
        assert!(
            user.text().contains("already has a lexer"),
            "{}",
            user.text()
        );
    }

    #[tokio::test]
    async fn a_reply_wrapped_in_prose_is_still_read() {
        let wrapped = format!("Sure, here is the plan:\n\n```json\n{GOOD}\n```\n\nGood luck!");
        let provider = Arc::new(RecordingProvider::new(vec![&wrapped]));
        let graph = planner(provider, 8)
            .plan("build a parser", None)
            .await
            .unwrap();
        assert_eq!(graph.nodes.len(), 2);
    }

    #[tokio::test]
    async fn a_bare_array_of_nodes_is_accepted() {
        let array = r#"[{"id":"a","objective":"do the thing"}]"#;
        let provider = Arc::new(RecordingProvider::new(vec![array]));
        let graph = planner(provider, 8)
            .plan("do the thing", None)
            .await
            .unwrap();
        assert_eq!(graph.nodes.len(), 1);
    }

    #[tokio::test]
    async fn a_cyclic_reply_fails_validation_after_one_retry() {
        let cyclic = r#"{"nodes":[
            {"id":"a","objective":"a","depends_on":["b"]},
            {"id":"b","objective":"b","depends_on":["a"]}
        ]}"#;
        let provider = Arc::new(RecordingProvider::new(vec![cyclic, cyclic]));
        let err = planner(Arc::clone(&provider), 8)
            .plan("build a parser", None)
            .await
            .unwrap_err();

        assert!(err.to_string().contains("cycle"), "{err}");
        assert_eq!(provider.requests().len(), 2, "exactly one retry");
    }

    #[tokio::test]
    async fn a_malformed_reply_is_retried_once_and_can_succeed() {
        let provider = Arc::new(RecordingProvider::new(vec!["not json at all", GOOD]));
        let graph = planner(Arc::clone(&provider), 8)
            .plan("build a parser", None)
            .await
            .unwrap();

        assert_eq!(graph.nodes.len(), 2);
        let requests = provider.requests();
        assert_eq!(requests.len(), 2, "one retry, no more");

        let retry_user = requests[1]
            .messages
            .iter()
            .rev()
            .find(|message| message.role == harness_core::Role::User)
            .unwrap();
        assert!(
            retry_user.text().contains("no JSON task graph"),
            "the parse error must be fed back: {}",
            retry_user.text()
        );
    }

    #[tokio::test]
    async fn two_malformed_replies_fail_and_quote_the_model() {
        let provider = Arc::new(RecordingProvider::new(vec!["garbage one", "garbage two"]));
        let err = planner(provider, 8)
            .plan("build a parser", None)
            .await
            .unwrap_err();

        assert!(err.to_string().contains("garbage two"), "{err}");
        assert!(err.to_string().contains("after one retry"), "{err}");
    }

    #[tokio::test]
    async fn too_many_nodes_are_refused() {
        let provider = Arc::new(RecordingProvider::new(vec![GOOD, GOOD]));
        let err = planner(provider, 1)
            .plan("build a parser", None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("more than the limit"), "{err}");
    }

    #[tokio::test]
    async fn an_empty_reply_is_retried_and_then_fails() {
        let provider = Arc::new(RecordingProvider::new(Vec::new()));
        let err = planner(Arc::clone(&provider), 8)
            .plan("build a parser", None)
            .await
            .unwrap_err();

        assert!(err.to_string().contains("no usable task graph"), "{err}");
        assert_eq!(provider.requests().len(), 2, "both attempts still ran");
    }
}
