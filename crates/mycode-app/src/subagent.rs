//! Role-aware `agent` host: catalog resolution, tool allowlists, isolation,
//! per-role model routes, and the parent-prompt delegation directive.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use mycode_agent::{Agent, AgentConfig, HookRunner};
use mycode_config::{
    AppSettings, HomeLayout, RoleCatalog, RoleIsolation, RoleThinking, SubagentRole,
    SubagentSettings, discover_roles,
};
use mycode_core::Message;
use mycode_providers::{ReqwestTransport, ResolvedProvider, WireProvider};
use mycode_tools::{ToolDyn, ToolRegistry};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

/// Subagent system brief: one-shot work, no user channel.
const SUBAGENT_SYSTEM_PROMPT: &str = "You are an MYCode subagent. Finish the brief with the \
tools you have. You cannot ask the user; reversible choices in the brief are authorized. \
Report assumptions that matter, then stop. \
The environment block names this OS and the one shell tool; write that shell's syntax.";

/// Parent-side tools a child can inherit when the role lists none.
///
/// `shell` is the stable allowlist token. The registered tool is still the
/// one interpreter resolved for this process (`powershell`, `bash`, or `cmd`).
const PARENT_TOOL_NAMES: &[&str] = &[
    "read",
    "write",
    "edit",
    "shell",
    "grep",
    "find",
    "web_search",
    "fetch_content",
];

/// Host for the `agent` tool: runs one nested agent on a resolved role.
pub(crate) struct BridgeAgentHost {
    resolved: ResolvedProvider,
    home: HomeLayout,
    cwd: PathBuf,
    settings: AppSettings,
    slots: Arc<Semaphore>,
    session_id: String,
    /// Child cancel tokens keyed by `session_id:call_id`.
    cancels: Arc<std::sync::Mutex<std::collections::HashMap<String, CancellationToken>>>,
    mcp_pool: Arc<tokio::sync::Mutex<Option<crate::mcp_tools::McpPool>>>,
    /// Shared Copilot bearer cache. A role routed at Copilot refreshes
    /// through the same slot as the parent turn.
    copilot: Arc<tokio::sync::Mutex<Option<(String, u64)>>>,
}

impl BridgeAgentHost {
    /// Binds the turn's provider, home, and subagent settings.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        resolved: ResolvedProvider,
        home: HomeLayout,
        cwd: PathBuf,
        settings: &AppSettings,
        session_id: String,
        cancels: Arc<std::sync::Mutex<std::collections::HashMap<String, CancellationToken>>>,
        mcp_pool: Arc<tokio::sync::Mutex<Option<crate::mcp_tools::McpPool>>>,
        copilot: Arc<tokio::sync::Mutex<Option<(String, u64)>>>,
    ) -> Self {
        let slots = settings.subagents.effective_concurrency() as usize;
        Self {
            resolved,
            home,
            cwd,
            settings: settings.clone(),
            slots: Arc::new(Semaphore::new(slots)),
            session_id,
            cancels,
            mcp_pool,
            copilot,
        }
    }

    fn cancel_key(session_id: &str, call_id: &str) -> String {
        format!("{session_id}:{call_id}")
    }
}

/// `git` for a worktree lease. On Windows the desktop process has no
/// console, so a plain spawn of `git.exe` allocates a black window.
fn git_command() -> std::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        // CREATE_NO_WINDOW: do not allocate a console for this child.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut command = std::process::Command::new("git");
        command.creation_flags(CREATE_NO_WINDOW);
        command
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("git")
    }
}

/// One git worktree lease: a disposable checkout plus its manifest, so a
/// crashed process can recover leases on the next start.
pub(crate) struct WorktreeLease {
    pub(crate) path: PathBuf,
    pub(crate) manifest: PathBuf,
}

impl WorktreeLease {
    /// Creates a detached worktree of the current repository HEAD.
    pub(crate) fn acquire(home: &HomeLayout, repo: &Path) -> Result<Self, String> {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or_default();
        let id = format!("agent-{}-{stamp}", std::process::id());
        let leases = home.root().join(WORKTREE_LEASE_DIR);
        std::fs::create_dir_all(&leases).map_err(|error| format!("lease dir: {error}"))?;
        let path = leases.join(&id);
        let output = git_command()
            .arg("-C")
            .arg(repo)
            .args(["worktree", "add", "--detach"])
            .arg(&path)
            .output()
            .map_err(|error| format!("git worktree: {error}"))?;
        if !output.status.success() {
            let _ = std::fs::remove_dir(&path);
            let reason = String::from_utf8_lossy(&output.stderr);
            return Err(format!("git worktree add failed: {}", reason.trim()));
        }
        let manifest = leases.join(format!("{id}.json"));
        let record = serde_json::json!({
            "repo": repo.to_string_lossy(),
            "path": path.to_string_lossy(),
        });
        std::fs::write(&manifest, record.to_string())
            .map_err(|error| format!("lease manifest: {error}"))?;
        Ok(Self { path, manifest })
    }

    /// Releases the lease; best-effort because the work may be done.
    pub(crate) fn release(self) {
        let output = git_command()
            .arg("-C")
            .arg(&self.path)
            .args(["worktree", "remove", "--force"])
            .arg(&self.path)
            .output();
        // Retry by repo path when the lease checkout itself is broken.
        if matches!(&output, Ok(result) if !result.status.success())
            && let Ok(record) = std::fs::read(&self.manifest)
            && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&record)
            && let Some(repo) = value["repo"].as_str()
        {
            let _ = git_command()
                .arg("-C")
                .arg(repo)
                .args(["worktree", "remove", "--force"])
                .arg(&self.path)
                .output();
        }
        let _ = std::fs::remove_file(&self.manifest);
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Captured diff cap handed back with a subagent result.
const MAX_WORKTREE_DIFF_BYTES: usize = 256 * 1024;

/// Reads the worktree diff while the checkout still exists.
pub(crate) fn capture_worktree_diff(path: &Path) -> String {
    let mut out = String::new();
    match git_command()
        .arg("-C")
        .arg(path)
        .args(["diff", "--binary", "--no-ext-diff", "HEAD"])
        .output()
    {
        Ok(output) if output.status.success() => {
            out.push_str(&String::from_utf8_lossy(&output.stdout));
        }
        Ok(output) => {
            out.push_str("worktree diff failed: ");
            out.push_str(String::from_utf8_lossy(&output.stderr).trim());
            out.push('\n');
        }
        Err(error) => {
            out.push_str(&format!("worktree diff failed: {error}\n"));
        }
    }
    match git_command()
        .arg("-C")
        .arg(path)
        .args(["status", "--porcelain", "-uall"])
        .output()
    {
        Ok(output) if output.status.success() => {
            for line in String::from_utf8_lossy(&output.stdout).lines() {
                let Some(name) = line.strip_prefix("?? ") else {
                    continue;
                };
                let name = name.trim();
                if name.is_empty() || name.contains("..") {
                    continue;
                }
                let file = path.join(name);
                out.push_str(&format!("diff --git a/{name} b/{name}\n"));
                match std::fs::read_to_string(&file) {
                    Ok(body) => {
                        out.push_str("new file\n--- /dev/null\n+++ b/");
                        out.push_str(name);
                        out.push('\n');
                        out.push_str(&body);
                        if !body.ends_with('\n') {
                            out.push('\n');
                        }
                    }
                    Err(error) => {
                        out.push_str(&format!("new file unreadable: {error}\n"));
                    }
                }
            }
        }
        Ok(output) => {
            out.push_str("worktree status failed: ");
            out.push_str(String::from_utf8_lossy(&output.stderr).trim());
            out.push('\n');
        }
        Err(error) => {
            out.push_str(&format!("worktree status failed: {error}\n"));
        }
    }
    cap_diff(out)
}

fn cap_diff(mut text: String) -> String {
    if text.len() <= MAX_WORKTREE_DIFF_BYTES {
        return text;
    }
    let mut end = MAX_WORKTREE_DIFF_BYTES;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text.push_str("\n... diff truncated\n");
    text
}

/// Joins a subagent answer with the diff captured from its worktree.
#[must_use]
pub(crate) fn format_subagent_handoff(result: &str, diff: &str) -> String {
    if diff.trim().is_empty() {
        return result.to_owned();
    }
    format!("{result}\n\n<worktree-diff>\n{diff}</worktree-diff>")
}

/// Captures the diff, then releases the worktree.
///
/// The diff is part of both the success string and the error string. The
/// checkout is removed only after that string is built.
pub(crate) fn handoff_worktree(
    lease: WorktreeLease,
    result: Result<String, mycode_tools::ToolError>,
) -> Result<String, mycode_tools::ToolError> {
    let diff = capture_worktree_diff(&lease.path);
    let outcome = match result {
        Ok(text) => Ok(format_subagent_handoff(&text, &diff)),
        Err(error) => Err(mycode_tools::ToolError::Execution(format_subagent_handoff(
            &error.to_string(),
            &diff,
        ))),
    };
    lease.release();
    outcome
}

/// Directory for worktree leases created by this build.
const WORKTREE_LEASE_DIR: &str = "agent-worktrees";
/// Lease directory written when the delegation tool was still named `task`.
const LEGACY_WORKTREE_LEASE_DIR: &str = "task-worktrees";

/// Removes leases left behind by a crashed process. Best-effort: a lease
/// whose repo is gone is simply deleted from disk.
///
/// Older builds stored leases under [`LEGACY_WORKTREE_LEASE_DIR`]. Both
/// directories are reclaimed so a rename does not orphan a checkout.
pub(crate) fn recover_agent_worktrees(home: &HomeLayout) {
    recover_worktree_lease_dir(home, WORKTREE_LEASE_DIR);
    recover_worktree_lease_dir(home, LEGACY_WORKTREE_LEASE_DIR);
}

fn recover_worktree_lease_dir(home: &HomeLayout, directory: &str) {
    let leases = home.root().join(directory);
    let Ok(entries) = std::fs::read_dir(&leases) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            if let Ok(bytes) = std::fs::read(&path)
                && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes)
                && let (Some(repo), Some(lease)) = (value["repo"].as_str(), value["path"].as_str())
            {
                let _ = git_command()
                    .arg("-C")
                    .arg(repo)
                    .args(["worktree", "remove", "--force"])
                    .arg(lease)
                    .output();
            }
            let _ = std::fs::remove_file(&path);
        } else {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

/// Live progress line `agent|role|phase|detail`. The prefix is the tool name.
fn agent_progress(role: &str, phase: &str, detail: &str) -> String {
    format!(
        "{}|{role}|{phase}|{detail}",
        mycode_tools::builtin::AGENT_PROGRESS_PREFIX
    )
}

/// Parent-prompt dispatch section for the roles that are actually enabled.
///
/// Catalog lines are the short when-to-use descriptions. The bullets are the
/// decision boundary: dispatch only for independent, bounded work that cuts
/// cost or improves quality, and run one artisan at a time unless the briefs
/// are independent. Only behavior this process implements is named.
#[must_use]
pub(crate) fn delegation_directive(catalog: &RoleCatalog, settings: &SubagentSettings) -> String {
    let enabled: Vec<&SubagentRole> = catalog
        .roles
        .iter()
        .filter(|role| settings.is_enabled(&role.name))
        .collect();
    if enabled.is_empty() {
        return String::new();
    }
    let catalog_lines = enabled
        .iter()
        .map(|role| role.catalog_line())
        .collect::<Vec<_>>()
        .join("\n");
    let dispatch = [
        "Use `agent` only when the work can run independently in parallel, the brief has clear boundaries, and doing so will actually cut cost or improve completion quality — not for trivial single-file work or vague wandering.",
        "`scout` when you need a repo, layout, API, or call-site map before deciding or editing; multi-file or unfamiliar exploration; or fact-gathering while you plan. It is read-only and stops after findings.",
        "`artisan` when the brief names files, outcome, and checks, or a chunk you can integrate while you stay orchestrator. It does not merge, commit, or open a PR. Expect a short outcome, paths, and what to verify — not a diff.",
        "At the same moment, do not fan out many parallel `artisan`s. Serialize when you can: one `artisan` at a time unless the briefs are clearly independent and you can integrate them separately.",
        "Do it yourself for a trivial single-file read, edit, typo, or one-liner; when you already have the context; or as a nested agent on the same brief. A vague ask gets a clarification or `scout` first, not an `artisan` sent to wander.",
        "Send one self-contained brief. The child has no parent conversation and returns once. Independent `agent` calls in one response run together, including with `search_tool` / `use_tool`.",
    ];
    let dispatch_block = dispatch
        .iter()
        .map(|line| format!("- {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "\n\n<dispatch>\nRoles:\n{catalog_lines}\n\nHow to dispatch:\n{dispatch_block}\n</dispatch>"
    )
}

/// Whether any catalog role is currently enabled for delegation.
#[must_use]
pub(crate) fn any_role_enabled(catalog: &RoleCatalog, settings: &SubagentSettings) -> bool {
    catalog
        .roles
        .iter()
        .any(|role| settings.is_enabled(&role.name))
}

/// Resolves isolation: an explicit request wins, worktree is refused for
/// read-only roles.
#[must_use]
pub(crate) fn resolve_isolation(role: &SubagentRole, requested: Option<&str>) -> RoleIsolation {
    let chosen = requested
        .and_then(RoleIsolation::parse)
        .unwrap_or(role.isolation);
    if chosen == RoleIsolation::Worktree && !role.is_write_capable() {
        RoleIsolation::Shared
    } else {
        chosen
    }
}

#[async_trait::async_trait]
impl mycode_tools::builtin::AgentHost for BridgeAgentHost {
    async fn run_subagent(
        &self,
        request: mycode_tools::builtin::SubagentRequest,
        progress: &mycode_tools::ToolStream,
        cancel: &CancellationToken,
        call_id: &str,
    ) -> Result<String, mycode_tools::ToolError> {
        let fail = |message: String| mycode_tools::ToolError::Execution(message);
        let catalog = discover_roles(&self.home, Some(&self.cwd));
        let role = catalog.role(&request.agent).cloned().ok_or_else(|| {
            fail(format!(
                "unknown role '{}'; available: {}",
                request.agent,
                catalog.names().join(", ")
            ))
        })?;
        if !self.settings.subagents.is_enabled(&role.name) {
            return Err(fail(format!(
                "role '{}' is disabled in settings",
                role.name
            )));
        }
        let isolation = resolve_isolation(&role, request.isolation.as_deref());
        let _ = progress.progress(agent_progress(&role.name, "queued", &request.description));
        let _ = progress.progress(agent_progress(&role.name, "prompt", &request.prompt));
        let permit = tokio::select! {
            permit = self.slots.acquire() => permit.map_err(|_| fail("agent slots closed".to_owned()))?,
            _ = cancel.cancelled() => return Err(fail("agent cancelled".to_owned())),
        };
        let lease = if isolation == RoleIsolation::Worktree {
            let home = self.home.clone();
            let cwd = self.cwd.clone();
            match tokio::task::spawn_blocking(move || WorktreeLease::acquire(&home, &cwd)).await {
                Ok(Ok(lease)) => Some(lease),
                Ok(Err(message)) => return Err(fail(message)),
                Err(error) => return Err(fail(format!("worktree task failed: {error}"))),
            }
        } else {
            None
        };
        let result = self
            .drive_subagent(&request, &role, progress, cancel, call_id, lease.as_ref())
            .await;
        let result = if let Some(lease) = lease {
            match tokio::task::spawn_blocking(move || handoff_worktree(lease, result)).await {
                Ok(outcome) => outcome,
                Err(error) => Err(fail(format!("worktree handoff failed: {error}"))),
            }
        } else {
            result
        };
        drop(permit);
        result
    }
}

impl BridgeAgentHost {
    /// Runs the nested agent until it finishes or the parent cancels.
    ///
    /// There is no wall-clock timeout. A long artisan or scout run stays
    /// alive until the model stops or the caller cancels.
    async fn drive_subagent(
        &self,
        request: &mycode_tools::builtin::SubagentRequest,
        role: &SubagentRole,
        progress: &mycode_tools::ToolStream,
        cancel: &CancellationToken,
        call_id: &str,
        lease: Option<&WorktreeLease>,
    ) -> Result<String, mycode_tools::ToolError> {
        let fail = |message: String| mycode_tools::ToolError::Execution(message);
        let run_dir = lease
            .map(|lease| lease.path.clone())
            .unwrap_or_else(|| self.cwd.clone());
        let resolved = self.resolve_route(&role.name).await.map_err(fail)?;
        let transport =
            ReqwestTransport::new().map_err(|_| fail("HTTP transport unavailable".to_owned()))?;
        let wire = WireProvider::new(resolved, Arc::new(transport));
        let allowed = role.resolve_tools(
            &PARENT_TOOL_NAMES
                .iter()
                .map(|name| (*name).to_owned())
                .collect::<Vec<_>>(),
        );
        let (mcp_tools, mcp_warning) = crate::mcp_tools::connect_mcp_tools(
            &self.home,
            &self.settings,
            &self.mcp_pool,
            Some(&self.cwd),
        )
        .await;
        let shell = mycode_tools::resolved_shell();
        let registry = Arc::new({
            let registry = child_registry_with(
                &self.home,
                &allowed,
                mycode_tools::ShellTool::forcing(shell.clone()),
            );
            if let Some(catalog) =
                crate::mcp_tools::McpCatalog::from_tools_with_warning(mcp_tools, mcp_warning)
            {
                for tool in catalog.direct_tools() {
                    if registry.get(tool.spec().name.as_str()).is_none() {
                        registry.register(tool);
                    }
                }
                registry.register(Arc::new(crate::mcp_tools::SearchTool::new(Arc::clone(
                    &catalog,
                ))));
                registry.register(Arc::new(crate::mcp_tools::UseTool::new(catalog)));
            }
            registry
        });
        let hooks = HookRunner::default();

        // A parent interrupt stops the child. Leaving it running held the
        // turn open, so Stop, send, and the window close never came back.
        let child_cancel = cancel.child_token();
        let run_cancel = child_cancel.clone();
        let cancel_key = Self::cancel_key(&self.session_id, call_id);
        if !call_id.is_empty()
            && let Ok(mut slots) = self.cancels.lock()
        {
            slots.insert(cancel_key.clone(), child_cancel.clone());
        }
        let (event_tx, mut event_rx) = tokio::sync::broadcast::channel(64);
        let role_name = role.name.clone();
        let progress_sink = progress.clone();
        let forwarder = tokio::spawn(async move {
            while let Ok(event) = event_rx.recv().await {
                match event {
                    mycode_core::events::AgentEvent::ToolStarted { name, target, .. } => {
                        let label = mycode_core::tool_label(&name, &target);
                        let _ = progress_sink.progress(agent_progress(&role_name, "tool", &label));
                    }
                    mycode_core::events::AgentEvent::ToolProgress { message, .. } => {
                        let _ =
                            progress_sink.progress(agent_progress(&role_name, "step", &message));
                    }
                    _ => {}
                }
            }
        });

        let path = run_dir.display().to_string();
        let _ = progress.progress(agent_progress(&role.name, "path", &path));
        let mut extra_roots = if lease.is_some() {
            Vec::new()
        } else {
            crate::turn::workspace_extra_roots(&self.home, &self.cwd)
        };
        extra_roots.sort();
        let system = subagent_system_prompt(
            &role,
            &registry,
            &run_dir,
            &extra_roots,
            std::env::consts::OS,
            std::env::consts::ARCH,
            &shell,
        );
        let mut config = AgentConfig::new()
            .with_system_prompt(system)
            .with_prompt_cache_key(Some(self.session_id.clone()));
        if let Some(level) = thinking_for(role, &self.settings.subagents).effort()
            && let Some(level) = mycode_core::ReasoningLevel::parse(level)
        {
            config = config.with_reasoning(level);
        }
        let mut agent = Agent::new(config);
        let prompt = Message::User(mycode_core::UserMessage::text(request.prompt.clone()));
        let role_name = role.name.clone();
        let run = tokio::spawn(async move {
            let env = mycode_agent::TurnEnv::new(&wire, &registry, &hooks)
                .with_cancel(run_cancel)
                .with_events(event_tx)
                .with_cwd(run_dir)
                .with_extra_roots(extra_roots);
            let outcome = agent.prompt(prompt, &env).await;
            forwarder.abort();
            if let Err(error) = outcome {
                return Err(fail(format!("subagent failed: {error}")));
            }
            let answer = agent
                .state()
                .messages()
                .iter()
                .rev()
                .find_map(|message| match message.as_ref() {
                    Message::Assistant(assistant) => {
                        let text = assistant.text();
                        (!text.trim().is_empty()).then_some(text)
                    }
                    _ => None,
                })
                .unwrap_or_default();
            if answer.trim().is_empty() {
                return Err(fail("subagent returned no answer".to_owned()));
            }
            Ok(answer)
        });
        let outcome = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                child_cancel.cancel();
                Err(fail(format!("subagent {role_name} cancelled")))
            }
            _ = child_cancel.cancelled() => {
                Err(fail(format!("subagent {role_name} cancelled")))
            }
            joined = run => joined.unwrap_or_else(|error| Err(fail(format!("subagent task failed: {error}")))),
        };
        if !call_id.is_empty()
            && let Ok(mut slots) = self.cancels.lock()
        {
            slots.remove(&cancel_key);
        }
        outcome
    }

    /// Resolves a per-role provider/model override, or inherits the turn.
    ///
    /// A routed role uses the same OAuth refresh and Copilot bearer exchange
    /// as the parent turn. The stored vault value is not sent as a bearer.
    async fn resolve_route(&self, role: &str) -> Result<ResolvedProvider, String> {
        let Some(entry) = self.settings.subagents.role(role) else {
            return Ok(self.resolved.clone());
        };
        let (Some(provider_id), Some(model)) = (entry.provider.as_deref(), entry.model.as_deref())
        else {
            return Ok(self.resolved.clone());
        };
        let provider = self
            .settings
            .providers
            .iter()
            .find(|provider| provider.id == provider_id && provider.enabled)
            .ok_or_else(|| format!("role '{role}' provider '{provider_id}' is missing"))?;
        let secrets = mycode_config::read_provider_secrets(&self.home)
            .map_err(|error| format!("role '{role}' secrets: {error}"))?;
        let key = secrets
            .key(provider_id)
            .ok_or_else(|| format!("role '{role}' has no API key for '{provider_id}'"))?;
        let (bearer, extra_headers) =
            crate::oauth::resolve_request_auth(&self.home, &self.copilot, provider, key)
                .await
                .map_err(|error| format!("role '{role}' auth: {error}"))?;
        let mut resolved = ResolvedProvider::resolve(
            provider,
            model,
            &bearer,
            &self.settings.effective_user_agent(),
        )
        .map_err(|error| format!("role '{role}' provider setup failed: {error:?}"))?;
        resolved.headers.extend(extra_headers);
        Ok(resolved)
    }
}

fn thinking_for(role: &SubagentRole, settings: &SubagentSettings) -> RoleThinking {
    settings
        .role(&role.name)
        .and_then(|entry| entry.thinking.as_deref())
        .and_then(RoleThinking::parse)
        .unwrap_or(role.thinking)
}

fn append_child_skills(system: &mut String, cwd: &Path, extras: &[PathBuf]) {
    let user_home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from);
    let mut skills = mycode_config::discover_skills(cwd, user_home.as_deref());
    for extra in extras {
        for skill in mycode_config::discover_skills(extra, None) {
            if skills.iter().any(|existing| existing.slug == skill.slug) {
                continue;
            }
            skills.push(skill);
        }
    }
    skills.sort_by(|left, right| {
        left.slug
            .cmp(&right.slug)
            .then_with(|| left.global.cmp(&right.global))
            .then_with(|| left.path.cmp(&right.path))
    });
    crate::turn::push_skill_catalog(system, skills);
}

/// System prompt for one child run. The shell block and the tool list name
/// the same interpreter the child can actually call.
fn subagent_system_prompt(
    role: &SubagentRole,
    registry: &ToolRegistry,
    run_dir: &Path,
    extra_roots: &[PathBuf],
    os: &str,
    arch: &str,
    shell: &mycode_tools::DetectedShell,
) -> String {
    let mut system = String::from(SUBAGENT_SYSTEM_PROMPT);
    system.push_str("\n\n# Role: ");
    system.push_str(&role.name);
    system.push_str("\n\n");
    system.push_str(&role.prompt);
    if !extra_roots.is_empty() {
        system.push_str("\n\nOther workspace folders (absolute paths only):\n");
        for root in extra_roots {
            system.push_str(&format!("- {}\n", root.display()));
        }
        if let Some(shell_tool) = registered_shell_name(registry) {
            system.push_str(&format!(
                "Relative paths stay in this run's cwd. For the other folders, pass an \
absolute path to `read`, `write`, `edit`, `find`, and `grep`, or an absolute \
path inside a `{shell_tool}` script (`mode` `script`). `{shell_tool}` starts in this run's \
cwd for both script and program mode.",
            ));
        }
    }
    system.push_str("\n\n");
    system.push_str(&mycode_tools::render_environment_block(
        os, arch, run_dir, shell,
    ));
    append_child_skills(&mut system, run_dir, extra_roots);
    if registry.get("search_tool").is_some() {
        system.push_str(
            "\n\nMCP tools are connected. Call `search_tool` with the tool name, then \
`use_tool` with arguments that match the returned inputSchema. Never guess parameters.",
        );
    }
    system.push_str("\n\n");
    system.push_str(&mycode_agent::build_system_prompt(registry));
    system
}

/// Model-facing name of the one shell tool in `registry`, if it has one.
fn registered_shell_name(registry: &ToolRegistry) -> Option<String> {
    let names = registry.names();
    ["powershell", "bash", "cmd"]
        .into_iter()
        .find(|name| names.iter().any(|registered| registered == name))
        .map(str::to_owned)
}

fn child_registry_with(
    home: &HomeLayout,
    allowed: &[String],
    shell: mycode_tools::ShellTool,
) -> ToolRegistry {
    let registry = ToolRegistry::new();
    let web_host: Arc<dyn mycode_tools::builtin::WebHost> =
        Arc::new(crate::tool_hosts::BridgeWebHost { home: home.clone() });
    let mut shell = Some(shell);
    for name in allowed {
        match name.as_str() {
            "read" => registry.register(Arc::new(mycode_tools::builtin::ReadTool)),
            "write" => registry.register(Arc::new(mycode_tools::builtin::WriteTool)),
            "edit" => registry.register(Arc::new(mycode_tools::builtin::EditTool)),
            "shell" | "bash" | "powershell" | "cmd" => {
                if let Some(tool) = shell.take() {
                    registry.register(Arc::new(tool));
                }
            }
            "grep" => registry.register(Arc::new(mycode_tools::builtin::GrepTool)),
            "find" => registry.register(Arc::new(mycode_tools::builtin::FindTool)),
            "web_search" => registry.register(Arc::new(mycode_tools::builtin::WebSearchTool::new(
                web_host.clone(),
            ))),
            "fetch_content" => registry.register(Arc::new(
                mycode_tools::builtin::FetchContentTool::new(web_host.clone()),
            )),
            _ => {}
        }
    }
    registry
}

#[cfg(test)]
mod tests {
    use super::{WorktreeLease, format_subagent_handoff, handoff_worktree};
    use mycode_config::{HomeLayout, SubagentSettings, builtin_roles};

    fn git(repo: &std::path::Path, args: &[&str]) {
        let output = super::git_command()
            .arg("-C")
            .arg(repo)
            .args(args)
            .env("GIT_AUTHOR_NAME", "mycode")
            .env("GIT_AUTHOR_EMAIL", "mycode@example.com")
            .env("GIT_COMMITTER_NAME", "mycode")
            .env("GIT_COMMITTER_EMAIL", "mycode@example.com")
            .output()
            .unwrap_or_else(|error| panic!("git {args:?}: {error}"));
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn handoff_includes_diff_before_the_worktree_is_removed() {
        let root = std::env::temp_dir().join(format!(
            "mycode-wt-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default()
        ));
        let repo = root.join("repo");
        let home_root = root.join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home_root).unwrap();
        git(&repo, &["init"]);
        std::fs::write(repo.join("tracked.txt"), "old\n").unwrap();
        git(&repo, &["add", "tracked.txt"]);
        git(&repo, &["commit", "-m", "init"]);
        let home = HomeLayout::from_root(&home_root).unwrap();
        let lease = WorktreeLease::acquire(&home, &repo).unwrap();
        let path = lease.path.clone();
        std::fs::write(path.join("tracked.txt"), "new\n").unwrap();
        std::fs::write(path.join("created.txt"), "created-body\n").unwrap();
        let text = handoff_worktree(lease, Ok("answer".to_owned())).unwrap();
        assert!(text.contains("answer"), "{text}");
        assert!(text.contains("created-body"), "{text}");
        assert!(text.contains("new"), "{text}");
        assert!(
            !path.exists(),
            "worktree was removed before the diff was captured"
        );
        let error = format_subagent_handoff("subagent failed", "diff-body");
        assert!(error.contains("subagent failed") && error.contains("diff-body"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn parent_dispatch_names_scout_and_artisan_only() {
        let text = super::delegation_directive(&builtin_roles(), &SubagentSettings::default());
        assert!(text.contains("`scout`"));
        assert!(text.contains("`artisan`"));
        assert!(text.contains("independently in parallel"));
        assert!(text.contains("clear boundaries"));
        assert!(text.contains("cut cost or improve completion quality"));
        assert!(text.contains("vague"));
        assert!(text.contains("not a diff"));
        assert!(text.contains("one `artisan` at a time"));
        assert!(text.contains("clearly independent"));
        assert!(!text.contains("steward"));
        assert!(!text.contains("sentinel"));
        assert!(!text.contains("`exec`"));
        assert!(!text.contains("`task`"));
    }

    fn shell_tool(kind: mycode_tools::ShellKind, program: &str) -> mycode_tools::ShellTool {
        mycode_tools::ShellTool::forcing(mycode_tools::DetectedShell {
            kind,
            program: std::path::PathBuf::from(program),
        })
    }

    fn shell_names(registry: &mycode_tools::ToolRegistry) -> Vec<String> {
        registry
            .names()
            .into_iter()
            .filter(|name| matches!(name.as_str(), "powershell" | "bash" | "cmd" | "shell"))
            .collect()
    }

    #[test]
    fn child_registry_keeps_one_shell_tool_for_every_allowlist_alias() {
        let home = HomeLayout::from_root(std::path::Path::new("/tmp/mycode-child-shell")).unwrap();
        let aliases = [
            "read".to_owned(),
            "shell".to_owned(),
            "bash".to_owned(),
            "powershell".to_owned(),
            "cmd".to_owned(),
        ];
        let cases = [
            (
                mycode_tools::ShellKind::WindowsPowerShell,
                "powershell.exe",
                "powershell",
            ),
            (mycode_tools::ShellKind::Pwsh, "pwsh.exe", "powershell"),
            (mycode_tools::ShellKind::Bash, "/bin/bash", "bash"),
            (mycode_tools::ShellKind::Cmd, "cmd.exe", "cmd"),
        ];
        for (kind, program, expected) in cases {
            let registry = super::child_registry_with(&home, &aliases, shell_tool(kind, program));
            assert_eq!(
                shell_names(&registry),
                vec![expected.to_owned()],
                "{kind:?}"
            );
        }
    }

    #[test]
    fn artisan_on_windows_powershell_does_not_receive_bash() {
        let home = HomeLayout::from_root(std::path::Path::new("/tmp/mycode-child-ps")).unwrap();
        let artisan = builtin_roles().role("artisan").expect("artisan").clone();
        let allowed = artisan.resolve_tools(
            &super::PARENT_TOOL_NAMES
                .iter()
                .map(|name| (*name).to_owned())
                .collect::<Vec<_>>(),
        );
        assert!(
            allowed.iter().any(|name| name == "shell"),
            "artisan inherits the shell allowlist token, got {allowed:?}"
        );
        let shell = mycode_tools::DetectedShell {
            kind: mycode_tools::ShellKind::WindowsPowerShell,
            program: std::path::PathBuf::from("powershell.exe"),
        };
        let registry = super::child_registry_with(
            &home,
            &allowed,
            mycode_tools::ShellTool::forcing(shell.clone()),
        );
        assert_eq!(shell_names(&registry), vec!["powershell".to_owned()]);
        let prompt = super::subagent_system_prompt(
            &artisan,
            &registry,
            std::path::Path::new("/work"),
            &[std::path::PathBuf::from("/other")],
            "windows",
            "x86_64",
            &shell,
        );
        assert!(prompt.contains("shell_tool: powershell"), "{prompt}");
        assert!(prompt.contains("Windows PowerShell 5.1"), "{prompt}");
        assert!(prompt.contains("Do not use `&&`"), "{prompt}");
        assert!(prompt.contains("here-string"), "{prompt}");
        assert!(prompt.contains("\n- powershell:"), "{prompt}");
        assert!(!prompt.contains("\n- bash:"), "{prompt}");
        assert!(!prompt.contains("\n- shell:"), "{prompt}");
        assert!(!prompt.contains("translated"), "{prompt}");
        assert!(
            prompt.contains("a `powershell` script"),
            "extra roots must name the active tool:\n{prompt}"
        );
    }

    #[test]
    fn a_bash_allowlist_entry_still_launches_the_active_powershell_tool() {
        let home = HomeLayout::from_root(std::path::Path::new("/tmp/mycode-child-alias")).unwrap();
        let mut role = builtin_roles().role("artisan").expect("artisan").clone();
        role.tools = Some(vec!["read".into(), "bash".into()]);
        let allowed = role.resolve_tools(&["read".into(), "shell".into()]);
        assert_eq!(allowed, vec!["read".to_owned(), "bash".to_owned()]);
        let shell = mycode_tools::DetectedShell {
            kind: mycode_tools::ShellKind::Pwsh,
            program: std::path::PathBuf::from("pwsh.exe"),
        };
        let registry = super::child_registry_with(
            &home,
            &allowed,
            mycode_tools::ShellTool::forcing(shell.clone()),
        );
        assert_eq!(shell_names(&registry), vec!["powershell".to_owned()]);
        let prompt = super::subagent_system_prompt(
            &role,
            &registry,
            std::path::Path::new(r"C:\work"),
            &[],
            "windows",
            "x86_64",
            &shell,
        );
        assert!(prompt.contains("shell_tool: powershell"), "{prompt}");
        assert!(prompt.contains("PowerShell 7"), "{prompt}");
        assert!(prompt.contains("`&&` and `||` are allowed"), "{prompt}");
        assert!(prompt.contains("\n- powershell:"), "{prompt}");
        assert!(!prompt.contains("\n- bash:"), "{prompt}");
        assert!(!prompt.contains("syntax errors"), "{prompt}");
        assert!(!prompt.contains("translated"), "{prompt}");
    }

    #[test]
    fn git_bash_and_cmd_subagents_match_that_interpreter() {
        let home =
            HomeLayout::from_root(std::path::Path::new("/tmp/mycode-child-gitbash")).unwrap();
        let artisan = builtin_roles().role("artisan").expect("artisan").clone();
        let allowed = ["read".to_owned(), "shell".to_owned()];
        let git_bash = mycode_tools::DetectedShell {
            kind: mycode_tools::ShellKind::Bash,
            program: std::path::PathBuf::from(r"C:\Program Files\Git\bin\bash.exe"),
        };
        let registry = super::child_registry_with(
            &home,
            &allowed,
            mycode_tools::ShellTool::forcing(git_bash.clone()),
        );
        assert_eq!(shell_names(&registry), vec!["bash".to_owned()]);
        let prompt = super::subagent_system_prompt(
            &artisan,
            &registry,
            std::path::Path::new(r"C:\work"),
            &[],
            "windows",
            "x86_64",
            &git_bash,
        );
        assert!(prompt.contains("shell_tool: bash"), "{prompt}");
        assert!(prompt.contains("Git Bash"), "{prompt}");
        assert!(prompt.contains("\n- bash:"), "{prompt}");
        assert!(!prompt.contains("\n- powershell:"), "{prompt}");
        assert!(!prompt.contains("translated"), "{prompt}");

        let cmd = mycode_tools::DetectedShell {
            kind: mycode_tools::ShellKind::Cmd,
            program: std::path::PathBuf::from(r"C:\Windows\System32\cmd.exe"),
        };
        let registry = super::child_registry_with(
            &home,
            &allowed,
            mycode_tools::ShellTool::forcing(cmd.clone()),
        );
        assert_eq!(shell_names(&registry), vec!["cmd".to_owned()]);
        let prompt = super::subagent_system_prompt(
            &artisan,
            &registry,
            std::path::Path::new(r"C:\work"),
            &[],
            "windows",
            "x86_64",
            &cmd,
        );
        assert!(prompt.contains("shell_tool: cmd"), "{prompt}");
        assert!(prompt.contains("cmd.exe"), "{prompt}");
        assert!(prompt.contains("\n- cmd:"), "{prompt}");
        assert!(!prompt.contains("translated"), "{prompt}");
    }

    #[test]
    fn scout_stays_read_only_while_the_prompt_names_the_active_shell() {
        let home = HomeLayout::from_root(std::path::Path::new("/tmp/mycode-child-scout")).unwrap();
        let scout = builtin_roles().role("scout").expect("scout").clone();
        let allowed = scout.resolve_tools(
            &super::PARENT_TOOL_NAMES
                .iter()
                .map(|name| (*name).to_owned())
                .collect::<Vec<_>>(),
        );
        assert!(
            allowed
                .iter()
                .all(|name| !matches!(name.as_str(), "shell" | "bash" | "powershell" | "cmd")),
            "scout must not inherit a shell tool, got {allowed:?}"
        );
        let shell = mycode_tools::DetectedShell {
            kind: mycode_tools::ShellKind::WindowsPowerShell,
            program: std::path::PathBuf::from("powershell.exe"),
        };
        let registry = super::child_registry_with(
            &home,
            &allowed,
            mycode_tools::ShellTool::forcing(shell.clone()),
        );
        assert!(shell_names(&registry).is_empty(), "{:?}", registry.names());
        let prompt = super::subagent_system_prompt(
            &scout,
            &registry,
            std::path::Path::new(r"C:\work"),
            &[],
            "windows",
            "x86_64",
            &shell,
        );
        assert!(prompt.contains("shell_tool: powershell"), "{prompt}");
        assert!(prompt.contains("Do not use `&&`"), "{prompt}");
        assert!(!prompt.contains("\n- powershell:"), "{prompt}");
        assert!(!prompt.contains("\n- bash:"), "{prompt}");
        assert!(!prompt.contains("translated"), "{prompt}");
    }

    #[test]
    fn zero_max_concurrent_uses_the_default() {
        let settings = SubagentSettings::default();
        assert_eq!(settings.max_concurrent, 0);
        assert_eq!(
            settings.effective_concurrency(),
            mycode_config::DEFAULT_SUBAGENT_CONCURRENCY
        );
        assert_ne!(settings.effective_concurrency(), 0);
        let mut explicit = settings;
        explicit.max_concurrent = 2;
        assert_eq!(explicit.effective_concurrency(), 2);
    }

    #[tokio::test]
    async fn a_routed_role_sends_the_oauth_access_token_not_the_vault_blob() {
        use std::sync::Arc;

        use mycode_config::{
            AppSettings, AuthorityRevision, HomeLayout, ProviderSecrets, ProviderSettings,
            SubagentRoleSettings, SubagentSettings, replace_provider_secrets,
        };
        use mycode_providers::{OAuthSecret, OPENAI_CODEX_PROVIDER_ID, ResolvedProvider};

        let root = std::env::temp_dir().join(format!(
            "mycode-subagent-auth-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let home = HomeLayout::from_root(&root).unwrap();
        let blob = OAuthSecret {
            kind: "oauth".to_owned(),
            access: "live-access-token".to_owned(),
            refresh: "refresh-token".to_owned(),
            expires: u64::MAX / 2,
            account_id: Some("acct_123".to_owned()),
        }
        .encode();
        replace_provider_secrets(
            &home,
            AuthorityRevision::ABSENT,
            &ProviderSecrets::new().with_key(OPENAI_CODEX_PROVIDER_ID, Some(&blob)),
        )
        .unwrap();
        let parent = ProviderSettings {
            id: "openai".to_owned(),
            kind: "openai-completions".to_owned(),
            base_url: "https://api.openai.com/v1".to_owned(),
            models: vec!["gpt-parent".to_owned()],
            enabled: true,
            context_limit: None,
            max_output: None,
        };
        let codex = ProviderSettings {
            id: OPENAI_CODEX_PROVIDER_ID.to_owned(),
            kind: "openai-responses".to_owned(),
            base_url: "https://chatgpt.com/backend-api/codex".to_owned(),
            models: vec!["gpt-5.5".to_owned()],
            enabled: true,
            context_limit: None,
            max_output: None,
        };
        let mut subagents = SubagentSettings::default();
        subagents.roles.push(SubagentRoleSettings {
            role: "scout".to_owned(),
            enabled: true,
            provider: Some(OPENAI_CODEX_PROVIDER_ID.to_owned()),
            model: Some("gpt-5.5".to_owned()),
            thinking: None,
        });
        let settings = AppSettings {
            providers: vec![parent.clone(), codex],
            subagents,
            ..AppSettings::default()
        };
        let resolved =
            ResolvedProvider::resolve(&parent, "gpt-parent", "parent-key", "ua").unwrap();
        let host = super::BridgeAgentHost::new(
            resolved,
            home,
            std::path::PathBuf::from("/tmp"),
            &settings,
            "session".to_owned(),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            Arc::new(tokio::sync::Mutex::new(None)),
            Arc::new(tokio::sync::Mutex::new(None)),
        );
        let routed = host.resolve_route("scout").await.expect("route");
        let authorization = routed
            .headers
            .iter()
            .find(|(name, _)| name == "authorization")
            .map(|(_, value)| value.as_str())
            .unwrap_or_default();
        assert_eq!(authorization, "Bearer live-access-token");
        assert!(!authorization.contains("refresh-token"));
        assert!(
            routed
                .headers
                .iter()
                .any(|(name, value)| name == "originator" && value == "pi")
        );
        assert!(
            routed
                .headers
                .iter()
                .any(|(name, value)| { name == "chatgpt-account-id" && value == "acct_123" })
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
