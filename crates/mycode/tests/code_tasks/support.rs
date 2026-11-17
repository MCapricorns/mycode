//! Fixed fixtures and independent outcome checks shared by offline and opt-in live runs.
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::future::Future;
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mycode::agent::{Agent, AgentConfig, HookRunner, TurnBudget, TurnEnv, build_system_prompt};
use mycode::core::{
    AssistantMessage, ContentBlock, EventStream, Message, Provider, ProviderError, Request,
    StopReason, StreamEvent, TextBlock, ToolCall, ToolResultMessage, ToolSpec, TurnOutcome,
    UserMessage,
};
use mycode::tools::builtin::{
    AgentActivity, AgentHost, AgentRuntime, AgentStateEvent, AgentStatusTool, EditTool, FindTool,
    GrepTool, ReadTool, SubagentRequest, WriteTool,
};
use mycode::tools::{ToolCtx, ToolDyn, ToolError, ToolRegistry, ToolResult, ToolStream};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

pub const REQUEST_LIMIT: u32 = 16;
const USER_NOTE: &str = "Uncommitted user note: preserve these bytes.\r\n";

pub struct Fixture {
    pub root: PathBuf,
    pub workspace: PathBuf,
    pub task: &'static str,
    pub events: Arc<Mutex<Vec<String>>>,
    pub lifetime: Arc<FixtureRoot>,
    pub children: Arc<AgentRuntime>,
    initial: BTreeMap<String, String>,
}

/// Detached child drivers and CodeMode registries retain this until their work ends.
pub struct FixtureRoot(PathBuf);

impl Drop for FixtureRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl Fixture {
    pub fn new(kind: &str) -> Self {
        let (task, files, oracle): (_, Vec<(&str, &str)>, _) = match kind {
            "clamp" => (
                include_str!("../fixtures/code_tasks/clamp/TASK.md"),
                vec![(
                    "clamp.rs",
                    include_str!("../fixtures/code_tasks/clamp/input.rs"),
                )],
                include_str!("../fixtures/code_tasks/clamp/oracle.rs"),
            ),
            "invoice" => (
                include_str!("../fixtures/code_tasks/invoice/TASK.md"),
                vec![
                    (
                        "invoice.rs",
                        include_str!("../fixtures/code_tasks/invoice/invoice.rs"),
                    ),
                    (
                        "policy.rs",
                        include_str!("../fixtures/code_tasks/invoice/policy.rs"),
                    ),
                ],
                include_str!("../fixtures/code_tasks/invoice/oracle.rs"),
            ),
            "ambiguous" => (
                include_str!("../fixtures/code_tasks/ambiguous/TASK.md"),
                vec![(
                    "ranges.rs",
                    include_str!("../fixtures/code_tasks/ambiguous/input.rs"),
                )],
                include_str!("../fixtures/code_tasks/ambiguous/oracle.rs"),
            ),
            _ => panic!("unknown fixture: {kind}"),
        };
        let root = std::env::temp_dir().join(format!("mycode-code-task-{}", uuid::Uuid::new_v4()));
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).expect("create isolated fixture");
        let initial: BTreeMap<_, _> = files
            .into_iter()
            .chain([("user-note.txt", USER_NOTE)])
            .map(|(name, text)| (name.to_owned(), text.to_owned()))
            .collect();
        for (name, text) in &initial {
            std::fs::write(workspace.join(name), text).expect("write fixture");
        }
        std::fs::write(root.join("oracle.rs"), oracle).expect("write independent oracle");
        Self {
            lifetime: Arc::new(FixtureRoot(root.clone())),
            children: Arc::new(AgentRuntime::new(root.join("artifacts"))),
            root,
            workspace,
            task,
            initial,
            events: Arc::default(),
        }
    }

    pub fn tools(&self) -> Arc<ToolRegistry> {
        self.build_tools(Some(self.events.clone()))
    }

    pub fn child_tools(&self) -> Arc<ToolRegistry> {
        self.build_tools(None)
    }

    pub async fn settle_children(&self) -> Result<(), String> {
        let status = AgentStatusTool::new(self.children.clone());
        let ctx = ToolCtx::new(&self.workspace);
        let (mut stream, _receiver) = ToolStream::channel();
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let result = status
                    .execute_dyn(serde_json::json!({}), &ctx, &mut stream)
                    .await
                    .map_err(|_| "child status unavailable")?;
                let states: Vec<AgentStateEvent> =
                    serde_json::from_str(&content_text(&result.content))
                        .map_err(|_| "child status malformed")?;
                if states.iter().all(|state| match state {
                    AgentStateEvent::Run(run) => run.status.is_terminal(),
                }) {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| {
            "child cleanup timed out; temporary directory remains owned by the driver".to_owned()
        })?
    }

    fn build_tools(&self, events: Option<Arc<Mutex<Vec<String>>>>) -> Arc<ToolRegistry> {
        let registry = Arc::new(ToolRegistry::new());
        let allowed: Arc<BTreeSet<_>> = Arc::new(self.initial.keys().cloned().collect());
        for inner in [
            Arc::new(ReadTool) as Arc<dyn ToolDyn>,
            Arc::new(EditTool),
            Arc::new(WriteTool),
            Arc::new(GrepTool),
            Arc::new(FindTool),
        ] {
            registry.register(Arc::new(FixtureTool {
                inner,
                allowed: allowed.clone(),
                events: events.clone(),
                _lifetime: self.lifetime.clone(),
            }));
        }
        registry
    }

    pub fn check_preserved_files(&self, changed: &[&str]) -> Result<(), String> {
        let actual: BTreeSet<_> = std::fs::read_dir(&self.workspace)
            .map_err(|_| "fixture directory became unreadable")?
            .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
            .collect::<Result<_, _>>()
            .map_err(|_| "fixture directory listing failed")?;
        if actual != self.initial.keys().cloned().collect() {
            return Err("unexpected added or removed fixture files".into());
        }
        for (name, initial) in &self.initial {
            if !changed.contains(&name.as_str())
                && std::fs::read(self.workspace.join(name)).map_err(|_| "fixture read failed")?
                    != initial.as_bytes()
            {
                return Err(format!("unrelated file changed: {name}"));
            }
        }
        Ok(())
    }

    pub async fn grade(&self, changed: &[&str]) -> Result<(), String> {
        self.check_preserved_files(changed)?;
        let executable = self.root.join(if cfg!(windows) {
            "oracle.exe"
        } else {
            "oracle"
        });
        let mut compile = tokio::process::Command::new("rustc");
        compile
            .args([
                "--test",
                "--edition=2024",
                "--crate-name",
                "code_task_oracle",
            ])
            .arg(self.root.join("oracle.rs"))
            .arg("-o")
            .arg(&executable);
        checked_process(compile, "compile oracle", &self.root).await?;
        let mut verify = tokio::process::Command::new(&executable);
        verify.arg("--test-threads=1");
        checked_process(verify, "run oracle", &self.root).await
    }
}

async fn checked_process(
    mut command: tokio::process::Command,
    label: &str,
    root: &Path,
) -> Result<(), String> {
    let stdout = root.join("oracle-stdout.txt");
    let stderr = root.join("oracle-stderr.txt");
    command.stdout(std::fs::File::create(&stdout).map_err(|_| "create oracle stdout")?);
    command.stderr(std::fs::File::create(&stderr).map_err(|_| "create oracle stderr")?);
    command.kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    let mut child = command
        .spawn()
        .map_err(|error| format!("{label}: {error}"))?;
    let status = match tokio::time::timeout(Duration::from_secs(60), child.wait()).await {
        Ok(status) => status.map_err(|error| format!("{label}: {error}"))?,
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(format!("{label} timed out"));
        }
    };
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "{label} failed:\n{}\n{}",
            std::fs::read_to_string(stdout).unwrap_or_default(),
            std::fs::read_to_string(stderr).unwrap_or_default()
        ))
    }
}

/// Restricts evaluation tools to known fixture paths before native preflight.
/// No shell, MCP, user configuration, or oracle files are exposed to the candidate.
struct FixtureTool {
    inner: Arc<dyn ToolDyn>,
    allowed: Arc<BTreeSet<String>>,
    events: Option<Arc<Mutex<Vec<String>>>>,
    _lifetime: Arc<FixtureRoot>,
}

impl ToolDyn for FixtureTool {
    fn spec(&self) -> ToolSpec {
        self.inner.spec()
    }
    fn parallel_safe(&self) -> bool {
        self.inner.parallel_safe()
    }
    fn execute_dyn<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolCtx,
        out: &'a mut ToolStream,
    ) -> Pin<Box<dyn Future<Output = Result<ToolResult, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            let raw = args.get("path").and_then(Value::as_str).unwrap_or(".");
            let mut relative = PathBuf::new();
            for part in Path::new(raw).components() {
                match part {
                    Component::CurDir => {}
                    Component::Normal(part) => relative.push(part),
                    _ => {
                        return Err(ToolError::InvalidArgs(
                            "evaluation requires a relative fixture path".into(),
                        ));
                    }
                }
            }
            let name = self.inner.spec().name;
            let path = relative.to_string_lossy();
            let search_root =
                relative.as_os_str().is_empty() && matches!(name.as_str(), "grep" | "find");
            if !search_root
                && (!self.allowed.contains(path.as_ref())
                    || (path == "user-note.txt" && matches!(name.as_str(), "write" | "edit")))
            {
                return Err(ToolError::InvalidArgs(
                    "evaluation tool is limited to permitted fixture files".into(),
                ));
            }
            let prepared = mycode::tools::prepare_tool_ctx(
                &ctx.cwd,
                &[],
                &[],
                ctx.cancel.clone(),
                &ctx.call_id,
                self.inner.as_ref(),
                &args,
            )
            .await
            .map_err(ToolError::InvalidArgs)?;
            let result = self.inner.execute_dyn(args, &prepared, out).await?;
            if !result.is_error
                && let Some(events) = &self.events
            {
                events.lock().unwrap().push(format!("{name}:{path}"));
            }
            Ok(result)
        })
    }
}

pub struct Frame {
    calls: Vec<ToolCall>,
    checks: Vec<(&'static str, bool, &'static str)>,
    answer: &'static str,
}

impl Frame {
    pub fn calls(calls: Vec<ToolCall>) -> Self {
        Self {
            calls,
            checks: vec![],
            answer: "",
        }
    }
    pub fn finish(answer: &'static str) -> Self {
        Self {
            calls: vec![],
            checks: vec![],
            answer,
        }
    }
    pub fn after(mut self, id: &'static str, error: bool, evidence: &'static str) -> Self {
        self.checks.push((id, error, evidence));
        self
    }
}

pub struct ScriptedProvider {
    frames: Mutex<VecDeque<Frame>>,
}
impl ScriptedProvider {
    pub fn new(frames: Vec<Frame>) -> Self {
        Self {
            frames: Mutex::new(frames.into()),
        }
    }
    pub fn assert_finished(&self) {
        assert!(
            self.frames.lock().unwrap().is_empty(),
            "script stopped before acceptance checks"
        );
    }
}
impl Provider for ScriptedProvider {
    fn stream<'a>(
        &'a self,
        request: &'a Request,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<EventStream, ProviderError>> + Send + 'a>> {
        Box::pin(async move {
            let frame = self
                .frames
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected extra provider request");
            for (id, error, evidence) in frame.checks {
                let result =
                    result_in(&request.messages, id).expect("required tool result missing");
                assert_eq!(
                    result.is_error,
                    error,
                    "tool call {id}: {}",
                    content_text(&result.content)
                );
                assert!(
                    content_text(&result.content).contains(evidence),
                    "tool call {id} omitted expected evidence: {evidence}"
                );
            }
            let message = AssistantMessage {
                stop_reason: if frame.calls.is_empty() {
                    StopReason::Stop
                } else {
                    StopReason::ToolUse
                },
                blocks: if frame.calls.is_empty() {
                    vec![ContentBlock::Text(TextBlock::new(frame.answer))]
                } else {
                    frame
                        .calls
                        .into_iter()
                        .map(ContentBlock::ToolCall)
                        .collect()
                },
                usage: None,
            };
            let (sender, stream) = EventStream::channel(cancel);
            sender.send(StreamEvent::Done { message }).await;
            Ok(stream)
        })
    }
}

pub fn call(id: &str, name: &str, arguments: Value) -> ToolCall {
    ToolCall::new(id, name, arguments)
}
pub fn content_text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect()
}
pub fn result_in<'a>(messages: &'a [Arc<Message>], id: &str) -> Option<&'a ToolResultMessage> {
    messages.iter().find_map(|message| match message.as_ref() {
        Message::ToolResult(result) if result.tool_call_id == id => Some(result),
        _ => None,
    })
}

pub async fn drive(
    provider: &dyn Provider,
    tools: &ToolRegistry,
    cwd: &Path,
    task: &str,
    budget: Arc<TurnBudget>,
    seed: Vec<Arc<Message>>,
    timeout: Duration,
) -> Result<Agent, String> {
    drive_with_cancel(
        provider,
        tools,
        cwd,
        task,
        budget,
        seed,
        timeout,
        CancellationToken::new(),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn drive_with_cancel(
    provider: &dyn Provider,
    tools: &ToolRegistry,
    cwd: &Path,
    task: &str,
    budget: Arc<TurnBudget>,
    seed: Vec<Arc<Message>>,
    timeout: Duration,
    cancel: CancellationToken,
) -> Result<Agent, String> {
    let mut system = format!(
        "Complete the fixture task with the available tools. Use exact relative paths. \
         Read before editing, preserve unrelated code, and verify the changed source. \
         Tool errors are evidence to inspect and recover from. Report only checks actually run.\n{}",
        build_system_prompt(tools),
    );
    if tools.get("agent").is_some() {
        system.push_str("\nThe enabled role is artisan: implement a bounded fixture change. \
Use isolation: shared for this evaluation. The child cannot delegate. The parent owns final review.");
    }
    let mut agent = Agent::new(
        AgentConfig::new()
            .with_system_prompt(system)
            .with_max_output_tokens(Some(8192)),
    );
    agent.seed_history(seed);
    let hooks = HookRunner::new();
    let cancel = cancel.child_token();
    let env = TurnEnv::new(provider, tools, &hooks)
        .with_cwd(cwd)
        .with_budget(budget)
        .with_cancel(cancel.clone());
    let activity = Arc::new(AgentActivity::default());
    let outcome = activity
        .scope(tokio::time::timeout(
            timeout,
            agent.prompt(Message::User(UserMessage::text(task)), &env),
        ))
        .await;
    // Even a normally finished answer must not leave yielded cells editing a
    // fixture while its oracle runs. Native workers settle before grading.
    cancel.cancel();
    activity.settled().await;
    let outcome = outcome
        .map_err(|_| "task wall-clock budget exhausted")?
        .map_err(|_| "agent stopped with a runtime error or exhausted its request budget")?;
    if outcome != TurnOutcome::Completed
        || agent.state().messages().iter().rev().find_map(|message| {
            if let Message::Assistant(assistant) = message.as_ref() {
                Some(assistant.stop_reason)
            } else {
                None
            }
        }) != Some(StopReason::Stop)
    {
        return Err("provider did not finish normally".into());
    }
    Ok(agent)
}

pub struct ChildHost {
    pub provider: Arc<dyn Provider>,
    pub tools: Arc<ToolRegistry>,
    pub cwd: PathBuf,
    pub budget: Arc<TurnBudget>,
    pub _lifetime: Arc<FixtureRoot>,
}
impl AgentHost for ChildHost {
    fn run_subagent<'a>(
        &'a self,
        request: SubagentRequest,
        _progress: &'a ToolStream,
        cancel: &'a CancellationToken,
        _call_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            if request.agent != "artisan" || request.isolation.as_deref() != Some("shared") {
                return Err(ToolError::InvalidArgs(
                    "evaluation supports artisan with shared isolation".into(),
                ));
            }
            let child = drive_with_cancel(
                self.provider.as_ref(),
                &self.tools,
                &self.cwd,
                &request.prompt,
                self.budget.clone(),
                vec![],
                Duration::from_secs(300),
                cancel.clone(),
            )
            .await
            .map_err(ToolError::Execution)?;
            Ok(child
                .state()
                .messages()
                .iter()
                .rev()
                .find_map(|message| match message.as_ref() {
                    Message::Assistant(assistant) => Some(assistant.text()),
                    _ => None,
                })
                .unwrap_or_default())
        })
    }
}

pub type Receipts = Arc<Mutex<Vec<ToolResult>>>;

pub struct RecordedAgentTool {
    pub inner: Arc<dyn ToolDyn>,
    pub receipts: Receipts,
    pub events: Arc<Mutex<Vec<String>>>,
}
impl ToolDyn for RecordedAgentTool {
    fn spec(&self) -> ToolSpec {
        self.inner.spec()
    }
    fn execute_dyn<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolCtx,
        out: &'a mut ToolStream,
    ) -> Pin<Box<dyn Future<Output = Result<ToolResult, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            let result = self.inner.execute_dyn(args, ctx, out).await?;
            if !result.is_error {
                self.events.lock().unwrap().push("handoff".into());
            }
            self.receipts.lock().unwrap().push(result.clone());
            Ok(result)
        })
    }
}

pub fn check_handoff(receipts: &Receipts, fixture: &Fixture) -> Result<(), String> {
    let receipts = receipts.lock().unwrap();
    let receipt = receipts
        .iter()
        .find(|result| !result.is_error && content_text(&result.content).contains("Review handoff"))
        .ok_or("no completed child handoff was inspected")?;
    let text = content_text(&receipt.content);
    let snapshot =
        &receipt.details.as_ref().ok_or("handoff details missing")?["agent_states"][0]["snapshot"];
    let id = snapshot["id"].as_str().ok_or("handoff run id missing")?;
    let artifact = Path::new(
        snapshot["artifact_path"]
            .as_str()
            .ok_or("handoff artifact missing")?,
    );
    if !text.contains(id)
        || !artifact.starts_with(fixture.root.join("artifacts"))
        || !artifact.is_file()
    {
        return Err("handoff has no usable run id or retained result artifact".into());
    }
    let events = fixture.events.lock().unwrap();
    let handoff = events
        .iter()
        .position(|event| event == "handoff")
        .ok_or("handoff event missing")?;
    if !events[handoff + 1..]
        .iter()
        .any(|event| event == "read:clamp.rs")
    {
        return Err("parent did not read the changed source after the child handoff".into());
    }
    Ok(())
}
