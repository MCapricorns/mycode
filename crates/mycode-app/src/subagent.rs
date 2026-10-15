//! Role-aware `agent` host: catalog resolution, tool allowlists, isolation,
//! per-role model routes, the parent-prompt delegation directive, and the
//! wall-clock limit on a running child.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use mycode_agent::{Agent, AgentConfig, HookRunner};
use mycode_config::{
    AppSettings, HomeLayout, RoleCatalog, RoleIsolation, RoleThinking, SubagentRole,
    SubagentSettings, discover_roles,
};
use mycode_core::Message;
use mycode_providers::{ReqwestTransport, ResolvedProvider, WireProvider};
use mycode_tools::ToolRegistry;
use tokio::sync::Semaphore;
use tokio::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

/// Subagent system brief: one-shot work, no user channel.
const SUBAGENT_SYSTEM_PROMPT: &str = "You are an MYCode subagent. Finish the brief with the \
tools you have. You cannot ask the user; reversible choices in the brief are authorized. \
Report assumptions that matter, then stop. Return only what the brief asks for. You cannot \
delegate further.";

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
        let stamp = crate::unix_now().as_millis();
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

/// Releases a lease the run never handed off.
///
/// A parent cancel stops polling a running `agent` call and drops it
/// mid-await. Without this guard the checkout stayed registered in the
/// user's repository until the next app start.
struct LeaseGuard(Option<WorktreeLease>);

impl LeaseGuard {
    fn path(&self) -> Option<&Path> {
        self.0.as_ref().map(|lease| lease.path.as_path())
    }
}

impl Drop for LeaseGuard {
    fn drop(&mut self) {
        let Some(lease) = self.0.take() else {
            return;
        };
        // `git worktree remove` blocks; keep it off the runtime thread.
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => drop(runtime.spawn_blocking(move || lease.release())),
            Err(_) => lease.release(),
        }
    }
}

/// Publishes one child's cancel token under `session_id:call_id` and
/// unpublishes it however the run ends, including a dropped call.
struct ChildCancelRegistration<'a> {
    cancels: &'a std::sync::Mutex<std::collections::HashMap<String, CancellationToken>>,
    call_id: &'a str,
    key: String,
}

impl<'a> ChildCancelRegistration<'a> {
    fn new(
        cancels: &'a std::sync::Mutex<std::collections::HashMap<String, CancellationToken>>,
        session_id: &str,
        call_id: &'a str,
        token: &CancellationToken,
    ) -> Self {
        let key = BridgeAgentHost::cancel_key(session_id, call_id);
        if !call_id.is_empty()
            && let Ok(mut slots) = cancels.lock()
        {
            slots.insert(key.clone(), token.clone());
        }
        Self {
            cancels,
            call_id,
            key,
        }
    }
}

impl Drop for ChildCancelRegistration<'_> {
    fn drop(&mut self) {
        forget_subagent_cancel(self.cancels, self.call_id, &self.key);
    }
}

/// Stops a spawned child run when its driver goes away, so a cancelled or
/// dropped call cannot keep editing files after its result was returned.
struct AbortOnDrop {
    cancel: CancellationToken,
    task: tokio::task::AbortHandle,
}

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.task.abort();
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
    // `core.quotepath=off` keeps non-ASCII file names as raw UTF-8 instead
    // of octal escapes; the escaped spelling would fail the `path.join`
    // lookup below on Windows and any non-ASCII path.
    match git_command()
        .arg("-c")
        .arg("core.quotepath=off")
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
    text.truncate(text.floor_char_boundary(MAX_WORKTREE_DIFF_BYTES));
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
    let cap = settings.effective_concurrency();
    let timeout_secs = settings.effective_timeout_secs();
    let dispatch = [
        "Use `agent` only when the work can run independently in parallel, the brief has clear boundaries, and doing so will actually cut cost or improve completion quality — not for trivial single-file work or vague wandering.",
        "You decide whether to delegate. Prefer `agent` with agent \"scout\" for web research, vendor docs, and a repository-wide map: independent scout calls run together in one response, and each returns a short map. Do not spend this chat on a long chain of `web_search` and `fetch_content` for that kind of task. A `fetch_content` result that reports HTTP 4xx or extract_failed is final for that URL. A narrow question does not need `scout`. Delegating broad research to scout is your judgment.",
        "`artisan` when the brief names files, outcome, and checks, or a chunk you can integrate while you stay orchestrator. It does not merge, commit, or open a PR. Expect a short outcome, paths, and what to verify — not a diff.",
        "At the same moment, do not fan out many parallel `artisan`s. Serialize when you can: one `artisan` at a time unless the briefs are clearly independent and you can integrate them separately.",
        "Do it yourself for a trivial single-file read, edit, typo, or one-liner; when you already have the context; or as a nested agent on the same brief. A vague ask gets a clarification or `scout` first, not an `artisan` sent to wander.",
    ];
    let limit = format!(
        "Send one self-contained brief. The child has no parent conversation and returns once. \
         Independent `agent` calls in one response run together. \
         Write-capable roles use a worktree, so concurrent artisans do not share the session checkout. \
         The host runs at most {cap} subagents at once. An artisan started past that cap waits, and the user is warned. \
         Each child stops after {timeout_secs} seconds of wall-clock time; time spent waiting for a slot does not count."
    );
    let dispatch_block = dispatch
        .iter()
        .map(|line| format!("- {line}"))
        .chain(std::iter::once(format!("- {limit}")))
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

impl mycode_tools::builtin::AgentHost for BridgeAgentHost {
    fn run_subagent<'a>(
        &'a self,
        request: mycode_tools::builtin::SubagentRequest,
        progress: &'a mycode_tools::ToolStream,
        cancel: &'a CancellationToken,
        call_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, mycode_tools::ToolError>> + Send + 'a>> {
        Box::pin(async move {
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
            // A parent interrupt stops the child. Leaving it running held the
            // turn open, so Stop, send, and the window close never came back.
            // The token is published before the run queues for a slot, so
            // the card's stop button also reaches a run that is still waiting.
            let child_cancel = cancel.child_token();
            let _registration = ChildCancelRegistration::new(
                &self.cancels,
                &self.session_id,
                call_id,
                &child_cancel,
            );
            let _ = progress.progress(agent_progress(&role.name, "queued", &request.description));
            let _ = progress.progress(agent_progress(&role.name, "prompt", &request.prompt));
            // `try_acquire` is the cap check: a waiter is past the limit.
            // Only an artisan raises the user-facing warning; scouts still wait.
            let permit = match self.slots.try_acquire() {
                Ok(permit) => permit,
                Err(tokio::sync::TryAcquireError::Closed) => {
                    return Err(fail("agent slots closed".to_owned()));
                }
                Err(tokio::sync::TryAcquireError::NoPermits) => {
                    if let Some(detail) = artisan_cap_detail(
                        &role.name,
                        0,
                        self.settings.subagents.effective_concurrency(),
                    ) {
                        let _ = progress.progress(agent_progress(&role.name, "capped", &detail));
                    }
                    tokio::select! {
                        permit = self.slots.acquire() => {
                            permit.map_err(|_| fail("agent slots closed".to_owned()))?
                        }
                        () = child_cancel.cancelled() => {
                            return Err(fail(format!("subagent {} cancelled", role.name)));
                        }
                    }
                }
            };
            let mut lease = LeaseGuard(if isolation == RoleIsolation::Worktree {
                let home = self.home.clone();
                let cwd = self.cwd.clone();
                match tokio::task::spawn_blocking(move || WorktreeLease::acquire(&home, &cwd)).await
                {
                    Ok(Ok(lease)) => Some(lease),
                    Ok(Err(message)) => return Err(fail(message)),
                    Err(error) => return Err(fail(format!("worktree task failed: {error}"))),
                }
            } else {
                None
            });
            let result = self
                .drive_subagent(
                    &request,
                    &role,
                    progress,
                    cancel,
                    &child_cancel,
                    lease.path(),
                )
                .await;
            let result = if let Some(lease) = lease.0.take() {
                match tokio::task::spawn_blocking(move || handoff_worktree(lease, result)).await {
                    Ok(outcome) => outcome,
                    Err(error) => Err(fail(format!("worktree handoff failed: {error}"))),
                }
            } else {
                result
            };
            drop(permit);
            result
        })
    }
}

impl BridgeAgentHost {
    /// Runs the nested agent until it finishes, the parent cancels, or the
    /// wall-clock budget elapses.
    ///
    /// The budget starts here, after a concurrency slot is held, so time
    /// spent queued does not count. A timeout cancels the child token and
    /// aborts the task.
    async fn drive_subagent(
        &self,
        request: &mycode_tools::builtin::SubagentRequest,
        role: &SubagentRole,
        progress: &mycode_tools::ToolStream,
        cancel: &CancellationToken,
        child_cancel: &CancellationToken,
        worktree: Option<&Path>,
    ) -> Result<String, mycode_tools::ToolError> {
        let fail = |message: String| mycode_tools::ToolError::Execution(message);
        let cancelled = || fail(format!("subagent {} cancelled", role.name));
        let started = Instant::now();
        let budget =
            Duration::from_secs(u64::from(self.settings.subagents.effective_timeout_secs()));
        let run_dir = worktree.map_or_else(|| self.cwd.clone(), Path::to_path_buf);
        // An OAuth refresh rotates the stored token, so routing always runs
        // to completion; the cancel is checked right after it.
        let resolved = self.resolve_route(&role.name).await.map_err(fail)?;
        if child_cancel.is_cancelled() {
            return Err(cancelled());
        }
        let transport =
            ReqwestTransport::new().map_err(|_| fail("HTTP transport unavailable".to_owned()))?;
        let wire = WireProvider::new(resolved, Arc::new(transport));
        let allowed = role.resolve_tools(
            &PARENT_TOOL_NAMES
                .iter()
                .map(|name| (*name).to_owned())
                .collect::<Vec<_>>(),
        );
        // Stdio servers are spawned kill-on-drop, so a stop may abandon a
        // slow handshake.
        let (mcp_tools, mcp_warning) = tokio::select! {
            biased;
            () = child_cancel.cancelled() => return Err(cancelled()),
            connected = crate::mcp_tools::connect_mcp_tools(
                &self.home,
                &self.settings,
                &self.mcp_pool,
                Some(&self.cwd),
            ) => connected,
        };
        let mcp_tools = if role.is_write_capable() {
            mcp_tools
        } else {
            mcp_tools
                .into_iter()
                .filter(|tool| tool.read_only())
                .collect()
        };
        let shell = mycode_tools::resolved_shell();
        let registry = Arc::new({
            let registry = child_registry_with(
                &self.home,
                &allowed,
                mycode_tools::ShellTool::forcing(shell.clone()),
            );
            crate::mcp_tools::register_mcp_tools(&registry, mcp_tools, mcp_warning);
            registry
        });
        let hooks = HookRunner::default();

        let run_cancel = child_cancel.clone();
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
        let mut extra_roots = if worktree.is_some() {
            Vec::new()
        } else {
            crate::turn::workspace_extra_roots(&self.home, &self.cwd)
        };
        extra_roots.sort();
        let read_roots = mycode_config::skill_read_roots(crate::turn::os_user_home().as_deref());
        let system = subagent_system_prompt(
            role,
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
        let remaining = budget.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            forwarder.abort();
            return Err(fail(subagent_timeout_message(&role_name, budget.as_secs())));
        }
        let run = tokio::spawn(async move {
            let env = mycode_agent::TurnEnv::new(&wire, &registry, &hooks)
                .with_cancel(run_cancel)
                .with_events(event_tx)
                .with_cwd(run_dir)
                .with_extra_roots(extra_roots)
                .with_read_roots(read_roots);
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
        // Also covers this future being dropped mid-await by a parent cancel.
        let _stop_run = AbortOnDrop {
            cancel: child_cancel.clone(),
            task: run.abort_handle(),
        };
        match finish_subagent_task(run, cancel, child_cancel, remaining).await {
            SubagentFinish::Cancelled => Err(cancelled()),
            SubagentFinish::TimedOut => {
                Err(fail(subagent_timeout_message(&role_name, budget.as_secs())))
            }
            SubagentFinish::Finished(result) => result,
        }
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

/// Progress detail when an artisan cannot take a concurrency slot.
///
/// Scouts share the semaphore and still wait, without this warning.
/// `permits_available` is `0` after `try_acquire` reports no permits.
fn artisan_cap_detail(role: &str, permits_available: usize, cap: u32) -> Option<String> {
    if role == "artisan" && permits_available == 0 {
        Some(cap.to_string())
    } else {
        None
    }
}

fn subagent_timeout_message(role: &str, secs: u64) -> String {
    format!("subagent {role} timed out after {secs}s")
}

/// How one nested agent task stopped.
#[derive(Debug)]
enum SubagentFinish {
    /// The parent turn or `CancelSubagent` fired.
    Cancelled,
    /// The wall-clock budget elapsed. The task was aborted.
    TimedOut,
    /// The task returned an answer or a tool error.
    Finished(Result<String, mycode_tools::ToolError>),
}

/// Waits for the nested task, the cancel tokens, or the remaining budget.
///
/// Parent cancel and an explicit child cancel win over a timeout that
/// becomes ready in the same poll. A finished task wins over the timeout.
/// A timeout cancels the child token and aborts the task so the run cannot
/// keep editing after the tool result has returned.
async fn finish_subagent_task(
    mut run: tokio::task::JoinHandle<Result<String, mycode_tools::ToolError>>,
    cancel: &CancellationToken,
    child_cancel: &CancellationToken,
    timeout: Duration,
) -> SubagentFinish {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            child_cancel.cancel();
            SubagentFinish::Cancelled
        }
        _ = child_cancel.cancelled() => SubagentFinish::Cancelled,
        joined = &mut run => SubagentFinish::Finished(joined.unwrap_or_else(|error| {
            Err(mycode_tools::ToolError::Execution(format!(
                "subagent task failed: {error}"
            )))
        })),
        _ = tokio::time::sleep(timeout) => {
            child_cancel.cancel();
            run.abort();
            SubagentFinish::TimedOut
        }
    }
}

fn forget_subagent_cancel(
    cancels: &std::sync::Mutex<std::collections::HashMap<String, CancellationToken>>,
    call_id: &str,
    cancel_key: &str,
) {
    if !call_id.is_empty()
        && let Ok(mut slots) = cancels.lock()
    {
        slots.remove(cancel_key);
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
    let mut skills = mycode_config::discover_skills(cwd, crate::turn::os_user_home().as_deref());
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
path inside a `{shell_tool}` script (`mode` `script`). `{shell_tool}` starts in this run's cwd.",
            ));
        }
    }
    system.push_str("\n\n");
    if registered_shell_name(registry).is_some() {
        system.push_str(
            "The environment block names this OS and the one shell tool; write that shell's syntax.\n\n",
        );
        system.push_str(&mycode_tools::render_environment_block(
            os, arch, run_dir, shell,
        ));
    } else {
        system.push_str(
            "This role has no shell tool. The environment block names the OS and cwd only.\n\n",
        );
        system.push_str(&render_os_block(os, arch, run_dir));
    }
    append_child_skills(&mut system, run_dir, extra_roots);
    if registry.get("search_tool").is_some() {
        system.push_str(
            "\n\nMCP tools are connected. For any MCP tool other than the direct ones, call \
`search_tool` with the tool name, then `use_tool` with arguments that match the returned \
inputSchema. Never guess parameters. A read-only role only receives MCP tools whose server set readOnlyHint.",
        );
    }
    system.push_str("\n\n");
    system.push_str(&mycode_agent::build_system_prompt(registry));
    system
}

fn render_os_block(os: &str, arch: &str, cwd: &Path) -> String {
    format!(
        "<environment>\nos: {os} ({arch})\ncwd: {}\n</environment>",
        cwd.display()
    )
}

/// Model-facing name of the one shell tool in `registry`, if it has one.
fn registered_shell_name(registry: &ToolRegistry) -> Option<String> {
    let names = registry.names();
    ["powershell", "bash", "zsh", "sh", "cmd"]
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
            "shell" | "bash" | "zsh" | "sh" | "powershell" | "cmd" => {
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
    use tokio::time::Duration;
    use tokio_util::sync::CancellationToken;

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
            crate::unix_now().as_nanos()
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

    #[tokio::test]
    async fn a_dropped_run_still_releases_its_worktree() {
        let root = std::env::temp_dir().join(format!(
            "mycode-wt-drop-{}-{}",
            std::process::id(),
            crate::unix_now().as_nanos()
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
        let manifest = lease.manifest.clone();
        // A parent cancel drops the `agent` call mid-await with the lease held.
        drop(super::LeaseGuard(Some(lease)));
        let released = tokio::time::timeout(Duration::from_secs(30), async {
            while path.exists() || manifest.exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        assert!(released.is_ok(), "the dropped lease stayed on disk");
        let listed = super::git_command()
            .arg("-C")
            .arg(&repo)
            .args(["worktree", "list", "--porcelain"])
            .output()
            .unwrap();
        let listed = String::from_utf8_lossy(&listed.stdout);
        assert_eq!(
            listed.matches("worktree ").count(),
            1,
            "the repository still lists the leased checkout: {listed}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn a_dropped_driver_stops_its_child_and_unpublishes_the_cancel() {
        let cancels = std::sync::Mutex::new(std::collections::HashMap::new());
        let child = CancellationToken::new();
        let registration = super::ChildCancelRegistration::new(&cancels, "ses", "call-1", &child);
        assert!(cancels.lock().unwrap().contains_key("ses:call-1"));
        let run = tokio::spawn(std::future::pending::<()>());
        let guard = super::AbortOnDrop {
            cancel: child.clone(),
            task: run.abort_handle(),
        };
        drop(guard);
        drop(registration);
        assert!(child.is_cancelled());
        let joined = tokio::time::timeout(Duration::from_secs(5), run)
            .await
            .expect("the child run kept going after its driver was dropped");
        assert!(joined.unwrap_err().is_cancelled());
        assert!(cancels.lock().unwrap().is_empty());
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
        assert!(text.contains("wall-clock"));
        assert!(text.contains("the user is warned"));
        assert!(text.contains(&format!(
            "at most {} subagents",
            mycode_config::DEFAULT_SUBAGENT_CONCURRENCY
        )));
        assert!(text.contains(&mycode_config::DEFAULT_SUBAGENT_TIMEOUT_SECS.to_string()));
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
            .filter(|name| {
                matches!(
                    name.as_str(),
                    "powershell" | "bash" | "zsh" | "sh" | "cmd" | "shell"
                )
            })
            .collect()
    }

    #[test]
    fn child_registry_keeps_one_shell_tool_for_every_allowlist_alias() {
        let home = HomeLayout::from_root(std::env::temp_dir().join("mycode-child-shell")).unwrap();
        let aliases = [
            "read".to_owned(),
            "shell".to_owned(),
            "bash".to_owned(),
            "zsh".to_owned(),
            "sh".to_owned(),
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
            (mycode_tools::ShellKind::Zsh, "/bin/zsh", "zsh"),
            (mycode_tools::ShellKind::Sh, "/bin/sh", "sh"),
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
        let home = HomeLayout::from_root(std::env::temp_dir().join("mycode-child-ps")).unwrap();
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
        assert!(prompt.contains("`powershell`"), "{prompt}");
        assert!(!prompt.contains("\n- bash:"), "{prompt}");
        assert!(!prompt.contains("translated"), "{prompt}");
        assert!(
            prompt.contains("a `powershell` script"),
            "extra roots must name the active tool:\n{prompt}"
        );
    }

    #[test]
    fn a_bash_allowlist_entry_still_launches_the_active_powershell_tool() {
        let home = HomeLayout::from_root(std::env::temp_dir().join("mycode-child-alias")).unwrap();
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
    fn zsh_and_cmd_subagents_match_that_interpreter() {
        let home = HomeLayout::from_root(std::env::temp_dir().join("mycode-child-zsh")).unwrap();
        let artisan = builtin_roles().role("artisan").expect("artisan").clone();
        let allowed = ["read".to_owned(), "shell".to_owned()];
        let zsh = mycode_tools::DetectedShell {
            kind: mycode_tools::ShellKind::Zsh,
            program: std::path::PathBuf::from("/bin/zsh"),
        };
        let registry = super::child_registry_with(
            &home,
            &allowed,
            mycode_tools::ShellTool::forcing(zsh.clone()),
        );
        assert_eq!(shell_names(&registry), vec!["zsh".to_owned()]);
        let prompt = super::subagent_system_prompt(
            &artisan,
            &registry,
            std::path::Path::new("/work"),
            &[],
            "macos",
            "aarch64",
            &zsh,
        );
        assert!(prompt.contains("shell_tool: zsh"), "{prompt}");
        assert!(prompt.contains("Write zsh"), "{prompt}");
        assert!(prompt.contains("The path separator is `/`."), "{prompt}");
        assert!(prompt.contains("\n- zsh:"), "{prompt}");
        assert!(!prompt.contains("\n- powershell:"), "{prompt}");
        assert!(!prompt.contains("Git Bash"), "{prompt}");
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
    fn scout_stays_read_only_and_does_not_list_a_shell() {
        let home = HomeLayout::from_root(std::env::temp_dir().join("mycode-child-scout")).unwrap();
        let scout = builtin_roles().role("scout").expect("scout").clone();
        let allowed = scout.resolve_tools(
            &super::PARENT_TOOL_NAMES
                .iter()
                .map(|name| (*name).to_owned())
                .collect::<Vec<_>>(),
        );
        assert!(
            allowed.iter().all(|name| {
                !matches!(
                    name.as_str(),
                    "shell" | "bash" | "zsh" | "sh" | "powershell" | "cmd"
                )
            }),
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
        assert!(
            !prompt.contains("shell_tool:"),
            "scout has no shell tool:\n{prompt}"
        );
        assert!(prompt.contains("no shell tool"), "{prompt}");
        assert!(prompt.contains("os: windows"), "{prompt}");
        assert!(!prompt.contains("Do not use `&&`"), "{prompt}");
        assert!(!prompt.contains("\n- powershell:"), "{prompt}");
        assert!(!prompt.contains("\n- bash:"), "{prompt}");
        assert!(!prompt.contains("translated"), "{prompt}");
        assert!(!prompt.contains("\n- shell:"), "{prompt}");
        assert!(registry.get("edit").is_none());
        assert!(prompt.contains("`goal`"), "{prompt}");
        assert!(
            !prompt.contains("Do not do that research inline"),
            "{prompt}"
        );
    }

    #[test]
    fn scout_prompt_lists_workspace_skills() {
        let root = std::env::temp_dir().join(format!(
            "mycode-scout-skill-{}-{}",
            std::process::id(),
            crate::unix_now().as_nanos()
        ));
        let skill = root.join(".agents").join("skills").join("aaa-scout");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(skill.join("SKILL.md"), "# Aaa Scout\nMap the crate.\n").unwrap();
        let home = HomeLayout::from_root(&root).unwrap();
        let scout = builtin_roles().role("scout").expect("scout").clone();
        let allowed = scout.resolve_tools(
            &super::PARENT_TOOL_NAMES
                .iter()
                .map(|name| (*name).to_owned())
                .collect::<Vec<_>>(),
        );
        let shell = mycode_tools::DetectedShell {
            kind: mycode_tools::ShellKind::Bash,
            program: std::path::PathBuf::from("/bin/bash"),
        };
        let registry = super::child_registry_with(
            &home,
            &allowed,
            mycode_tools::ShellTool::forcing(shell.clone()),
        );
        let prompt =
            super::subagent_system_prompt(&scout, &registry, &root, &[], "linux", "x86_64", &shell);
        assert!(prompt.contains("/aaa-scout"), "{prompt}");
        assert!(prompt.contains("read the matching SKILL.md"), "{prompt}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn artisan_gets_the_same_tool_guidance_as_the_parent() {
        let home =
            HomeLayout::from_root(std::env::temp_dir().join("mycode-child-artisan")).unwrap();
        let artisan = builtin_roles().role("artisan").expect("artisan").clone();
        let allowed = artisan.resolve_tools(
            &super::PARENT_TOOL_NAMES
                .iter()
                .map(|name| (*name).to_owned())
                .collect::<Vec<_>>(),
        );
        assert!(allowed.iter().any(|name| name == "edit"), "{allowed:?}");
        let shell = mycode_tools::DetectedShell {
            kind: mycode_tools::ShellKind::Bash,
            program: std::path::PathBuf::from("/bin/bash"),
        };
        let registry = super::child_registry_with(
            &home,
            &allowed,
            mycode_tools::ShellTool::forcing(shell.clone()),
        );
        let prompt = super::subagent_system_prompt(
            &artisan,
            &registry,
            std::path::Path::new("/work"),
            &[],
            "linux",
            "x86_64",
            &shell,
        );
        assert!(prompt.contains("<tool_calling>"), "{prompt}");
        assert!(prompt.contains("`write`, `edit`, and `bash`"), "{prompt}");
        assert!(prompt.contains("cannot delegate further"), "{prompt}");
        assert!(
            !prompt.contains("Do not do that research inline"),
            "{prompt}"
        );
        let directive = super::delegation_directive(
            &builtin_roles(),
            &mycode_config::SubagentSettings::default(),
        );
        assert!(directive.contains("your judgment"), "{directive}");
        assert!(directive.contains("scout"), "{directive}");
        assert!(
            !directive.contains("Do not do that research inline"),
            "{directive}"
        );
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

    #[test]
    fn only_an_artisan_with_no_permit_is_past_the_cap() {
        assert_eq!(
            super::artisan_cap_detail("artisan", 0, 4).as_deref(),
            Some("4")
        );
        assert!(super::artisan_cap_detail("scout", 0, 4).is_none());
        assert!(super::artisan_cap_detail("artisan", 1, 4).is_none());
        assert_eq!(
            super::agent_progress("artisan", "capped", "4"),
            "agent|artisan|capped|4"
        );
        assert_eq!(
            super::subagent_timeout_message("scout", 1200),
            "subagent scout timed out after 1200s"
        );
    }

    #[tokio::test]
    async fn wall_clock_timeout_aborts_a_run_that_does_not_finish() {
        let cancel = CancellationToken::new();
        let child = CancellationToken::new();
        let run = tokio::spawn(async {
            std::future::pending::<()>().await;
            Ok::<String, mycode_tools::ToolError>("never".to_owned())
        });
        let outcome =
            super::finish_subagent_task(run, &cancel, &child, Duration::from_millis(40)).await;
        assert!(matches!(outcome, super::SubagentFinish::TimedOut));
        assert!(child.is_cancelled());
    }

    #[tokio::test]
    async fn a_finished_run_beats_the_wall_clock() {
        let cancel = CancellationToken::new();
        let child = CancellationToken::new();
        let run = tokio::spawn(async { Ok::<String, mycode_tools::ToolError>("done".to_owned()) });
        let outcome =
            super::finish_subagent_task(run, &cancel, &child, Duration::from_secs(2)).await;
        match outcome {
            super::SubagentFinish::Finished(Ok(text)) => assert_eq!(text, "done"),
            other => panic!("expected a finished run, got {other:?}"),
        }
        assert!(!child.is_cancelled());
    }

    #[tokio::test]
    async fn parent_cancel_stops_the_run_before_the_wall_clock() {
        let cancel = CancellationToken::new();
        let child = CancellationToken::new();
        cancel.cancel();
        let run = tokio::spawn(async {
            std::future::pending::<()>().await;
            Ok::<String, mycode_tools::ToolError>("no".to_owned())
        });
        let outcome =
            super::finish_subagent_task(run, &cancel, &child, Duration::from_secs(30)).await;
        assert!(matches!(outcome, super::SubagentFinish::Cancelled));
        assert!(child.is_cancelled());
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
            crate::unix_now().as_nanos()
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
