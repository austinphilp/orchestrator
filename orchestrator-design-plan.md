# Orchestrator: Final Product Design Plan (Rust + ratatui, OpenCode harness, Linear tickets, OpenRouter supervisor)

This document is a “build spec” for the orchestrator/supervisor you described: an inbox-first, vim-keybind, keyboard-only TUI that supervises multiple autonomous coding sessions (OpenCode today, Codex later), minimizes context switching, and keeps “in-flight work” moving without you micromanaging commands.

It’s designed to be extensible across:
- **Harnesses** (OpenCode, Codex, Claude Code, etc.)
- **Ticket systems** (Linear first, Jira/GitHub Issues later)
- **Git providers** (GitHub first via `gh`, others later)
- **LLM providers** (OpenRouter first, OpenAI/Anthropic/local later)
- **Repo layouts** (single repo, multiple repos, monorepos, multi-worktree)

---

## 1) Current repo snapshot and intended evolution

Your repo already has a good “composition root + adapters” seed:

**Workspace**
- root `Cargo.toml` is a workspace with `crates/*` and shared dependencies (tokio, ratatui, crossterm, serde, tracing).

**Crates (present)**
- `orchestrator-app`: composition root, reads config from env TOML, wires `OpenRouterSupervisor`, `GhCliClient`, and runs a minimal UI.
- `orchestrator-core`: basic domain types + `Supervisor` and `GithubClient` traits.
- `orchestrator-ui`: a minimal ratatui loop that renders status and quits on `q`.
- `orchestrator-supervisor`: OpenRouter API key loading + `health_check`.
- `orchestrator-github`: `gh auth status` healthcheck via an abstracted command runner.

**What’s missing vs final product**
- A real **domain model** (tickets/sessions/worktrees/inbox items)
- A **WorkerBackend abstraction** (OpenCode sessions, Codex later)
- PTY + terminal emulator embedding
- Attention router (inbox engine)
- Config-driven keymaps + which-key overlay
- Linear integration (ticket sync + state updates)
- Worktree lifecycle and PR automation
- Streaming OpenRouter queries + supervisor UI

The plan below expands the architecture while keeping the same “adapter-first” style.

---

## 2) Product goals and non-goals

### Goals
1) **Inbox-first workflow**  
   The default view is an *attention inbox* of “things that need you” across all sessions.

2) **Vimlike keyboard control everywhere**  
   Mode-based (Normal/Insert/Terminal), prefix keymaps, and a which-key legend.

3) **Session terminal on demand**  
   You can “drop into” the live OpenCode terminal in the center pane, then minimize it back to the background without killing the session.

4) **Supervisor agent interface**  
   You can ask arbitrary natural-language questions about any in-flight item (“What is session X doing right now?”, “Why blocked?”, “Summarize changes since I last looked”), and common queries are bound to keys.

5) **Automation of the workflow**  
   Ticket → plan → implement → tests → draft PR → you review → mark ready → request reviewers → close loop. The orchestrator bothers you only at gates.

6) **Extensibility**  
   Adding a new harness provider or ticket system should not require UI rewrites.

### Non-goals (for sanity)
- Not trying to replace GitHub/Linear UIs fully.
- Not trying to be tmux: it embeds terminals, but the primary UX is attention routing + workflow.
- Not trying to be “the planner”: OpenCode does the planning; orchestrator is the supervisor/router.

---

## 3) Core architecture: event-driven supervisor

The orchestrator should be thought of as **four cooperating subsystems**:

1) **Runtime / Workers**  
   Spawns harness sessions (OpenCode) in worktrees and reads their output streams.

2) **Normalization layer**  
   Turns harness outputs into structured events (checkpoints, blocked, needs-input, artifacts).

3) **Attention engine**  
   Converts events into inbox items, prioritizes/batches them, and tracks workflow gates.

4) **TUI shell**  
   Renders state and routes inputs using config-driven vimlike keymaps; can spawn supervisor queries.

### Dataflow diagram

```
        +-----------------+
        |  TUI (ratatui)  |
        | inbox/focus/tty |
        +--------+--------+
                 |
                 | commands / keybinds / NL queries
                 v
+----------------+------------------+
| Orchestrator Core (domain/state)  |
| - event store                    |
| - attention engine               |
| - workflow FSM                   |
| - command registry               |
+---------+-------------+----------+
          |             |
          |             | supervisor queries (OpenRouter streaming)
          |             v
          |     +---------------+
          |     | Supervisor    |
          |     | LLM Provider  |
          |     +---------------+
          |
          | worker lifecycle + PTY IO
          v
+-------------------------------+
| Worker Manager                |
| - spawn PTY                   |
| - route stdin                 |
| - resize                      |
| - read stdout                 |
+---------------+---------------+
                |
                | raw output
                v
       +-------------------+
       | Normalizer        |
       | (protocol parser) |
       +-------------------+
                |
                | structured events
                v
         (back into core)
```

---

## 4) Crate layout for the final system

You can keep your existing crates and add a few, or refactor names. A clean split:

### `orchestrator-core` (domain + command APIs)
**Owns**
- Domain types (Ticket, WorkItem, Worktree, WorkerSession, InboxItem, Artifact, WorkflowState)
- Event types + EventStore trait
- Command registry: stable command IDs + argument schema
- Capability types and traits (WorkerBackend trait, Ticketing trait, Vcs trait, RepoProvider trait, LlmProvider trait)
- Attention engine + Workflow FSM (state machine)

### `orchestrator-ui` (ratatui shell)
**Owns**
- View stack (InboxView, FocusCardView, TerminalView, InspectorView, CommandPaletteView)
- Input modes + input router
- Keymap trie + which-key overlay
- Rendering and incremental updates from state

### `orchestrator-runtime` (process + PTY + concurrency)
**Owns**
- PTY spawn/IO
- Terminal screen buffer / emulator integration
- WorkerManager: track active sessions, backgrounding, multiplexing, throttling
- Event ingestion loop(s)

### `backend-opencode`
**Owns**
- Implementation of WorkerBackend using OpenCode CLI in a PTY
- Protocol expectations (“Supervisor Protocol”) and tagging guidance
- Optional “export session” support (if OpenCode provides)

### `integration-linear`
**Owns**
- Linear client (auth, sync issues, update state, comments)
- Ticket mapping: 1 Linear issue ↔ 1 work item ↔ 1 worktree/session

### `integration-git`
**Owns**
- Worktree creation/deletion
- Branch naming scheme
- Repo discovery under configured roots
- Safety rails (never delete unmerged branches unless configured)

### `integration-github`
**Owns**
- GitHub adapter via `gh` CLI (create draft PR, mark ready, request reviewers, query review-needed)
- URL opening adapter (`open`/`xdg-open`) behind a trait

### `orchestrator-supervisor`
**Owns**
- OpenRouter client with **streaming**
- SupervisorQuery engine: retrieve bounded context from EventStore and craft prompts
- Template queries invoked by keybind macros

### `config`
**Owns**
- TOML config schema, load/merge (global + per-project overrides)
- Keymap config parsing and validation
- Command templates
- Project roots, repo settings, workflow toggles

---

## 5) Domain model (the “truth” the UI renders)

### 5.1 Core entities

**Project**
- `id`
- `name`
- `roots: Vec<PathBuf>` (repo roots)
- `settings` (per-project overrides)

**Ticket (Linear issue)**
- `provider_id` (Linear UUID)
- `identifier` (e.g., AP-123)
- `title`
- `state` (Linear workflow state name/id)
- `url`
- `priority`, `labels`
- `updated_at`

**WorkItem (1:1 with ticket in your chosen model)**
- `id`
- `ticket_ref`
- `repo_ref`
- `worktree: WorktreeRef`
- `session: WorkerSessionRef`
- `workflow_state: WorkflowState`
- `attention_items: Vec<InboxItemId>`
- `artifacts: Vec<Artifact>`

**Worktree**
- `path`
- `branch`
- `base_branch`
- `created_at`
- `status` (clean/dirty, commits ahead, etc.)

**WorkerSession**
- `backend_kind` (OpenCode, Codex, …)
- `workdir` (worktree path)
- `pty_handle` (runtime-only)
- `screen_buffer` (terminal view snapshot)
- `status` (Running / WaitingForUser / Blocked / Done / Crashed)
- `last_checkpoint` (optional structured)

**InboxItem**
- `id`
- `work_item_id`
- `kind`: `NeedsDecision | NeedsApproval | Blocked | FYI | ReadyForReview`
- `summary` (one-liner)
- `created_at`, `updated_at`
- `priority_score`
- `requires_user_input: bool`
- `proposed_response` (optional)
- `evidence_refs` (links to artifacts/log snippets)

**Artifact**
- `kind`: `Diff | PR | TestRun | LogSnippet | Link | Export`
- `metadata` (diffstat, pr url, test status)
- `storage_ref` (in db or file)

### 5.2 Event model (append-only)
Make everything that happens an event so:
- supervisor Q&A can answer from history
- UI can reconstruct state after restart
- debugging is tractable

Event types (examples):
- `TicketSynced`
- `WorkItemCreated`
- `WorktreeCreated`
- `SessionSpawned`
- `SessionCheckpoint { label, payload }`
- `SessionNeedsInput { prompt_id, prompt, options }`
- `SessionBlocked { reason, hint }`
- `ArtifactCreated { kind, ref }`
- `WorkflowTransition { from, to, reason }`
- `InboxItemCreated/Resolved`
- `UserResponded { prompt_id, response }`

---

## 6) WorkerBackend abstraction (harnesses)

### 6.1 Backend trait
Design it around **capabilities**. Don’t force Codex/OpenCode to look identical.

```rust
pub struct BackendCapabilities {
  pub structured_events: bool,
  pub session_export: bool,
  pub diff_provider: bool,
  pub supports_background: bool, // practical “can keep running while not focused”
}

#[async_trait]
pub trait WorkerBackend: Send + Sync {
  fn kind(&self) -> BackendKind;
  fn capabilities(&self) -> BackendCapabilities;

  async fn spawn(&self, spec: SpawnSpec) -> Result<SessionHandle, BackendError>;
  async fn kill(&self, session: &SessionHandle) -> Result<(), BackendError>;

  async fn send_input(&self, session: &SessionHandle, input: &[u8]) -> Result<(), BackendError>;
  async fn resize(&self, session: &SessionHandle, cols: u16, rows: u16) -> Result<(), BackendError>;

  fn subscribe(&self, session: &SessionHandle) -> EventStream<BackendEvent>;
  fn snapshot(&self, session: &SessionHandle) -> Result<TerminalSnapshot, BackendError>;
}
```

### 6.2 OpenCode backend strategy
You want the orchestrator to *lean on OpenCode heavily*, so the backend shouldn’t micromanage. It should:
- spawn the OpenCode CLI inside a PTY at the worktree path
- stream output continuously
- parse “structured tags” (see below) when present
- fall back to lightweight heuristics otherwise
- never block the UI while reading output

---

## 7) The Supervisor Protocol (how to avoid fragile log parsing)

Your “focus card” UX needs reliable signals like:
- “I need you to choose A/B”
- “I’m blocked on failing tests”
- “PR draft created at URL”
- “I’m currently refactoring X”

Relying on raw TUI output is brittle. Instead, enforce a minimal protocol that the harness emits.

### 7.1 Tag format (line-oriented)
Use a single-line prefix that’s unlikely to appear in code:

- `@@checkpoint <json>`
- `@@needs_input <json>`
- `@@blocked <json>`
- `@@artifact <json>`
- `@@done <json>`

Example payloads:

```text
@@checkpoint {"stage":"implementing","detail":"updating RateLimiter","files":["src/rl.rs"]}
@@needs_input {"id":"q1","kind":"choice","question":"Choose API shape","options":["A","B"],"default":"A"}
@@artifact {"kind":"pr","url":"https://github.com/.../pull/12","draft":true}
@@blocked {"reason":"tests failing","hint":"run `cargo test -p foo` and inspect output","log_ref":"artifact:log:abc123"}
```

### 7.2 Where tags come from
Two options (use both):
1) A “skill / instruction” you always prepend when starting a session:  
   “Emit @@checkpoint/@@needs_input/@@artifact lines whenever relevant.”
2) Orchestrator can periodically inject:  
   “Emit a checkpoint now.”

### 7.3 Normalization rules
The normalizer should:
- parse tag lines into structured backend events
- store full raw output separately (bounded / rolled)
- attach artifacts (PR URLs, test outputs, diffs) as stable references
- create or update InboxItems based on event type

---

## 8) Attention engine (the heart of “minimize context switching”)

Your attention engine turns events into *interruptions* only when necessary.

### 8.1 Inbox item types and creation rules

**NeedsDecision**
- created on `SessionNeedsInput` with `kind=choice|clarification`
- priority high, blocks worker progress

**Blocked**
- created on `SessionBlocked`
- priority high, but may be auto-triaged (e.g., rerun tests, pull latest)

**NeedsApproval**
- created when workflow enters a gate that you’ve configured as “human-only”
- example: “convert draft PR to ready”

**ReadyForReview**
- created when:
  - tests passed
  - diff ready
  - PR draft exists
- This can be batched across items.

**FYI digest**
- created when there’s meaningful progress but no action required
- should *not* interrupt—only visible if you open it.

### 8.2 Priority scoring
Priority should be deterministic and tunable:

```
score = base(kind)
      + age_weight * minutes_since_created
      + idle_risk_weight * worker_idle_risk
      + workflow_gate_bonus
      + user_pinned_bonus
```

Where `worker_idle_risk` increases when:
- worker is waiting for input
- worker has paused progress for >N minutes
- or worker is blocked and no action taken

### 8.3 Batching
Batch interrupts to reduce switching:
- group by kind: “3 items need decision”
- group by repo: “2 items in repo X”
- group by action: “PR reviews pending”

---

## 9) Workflow automation (ticket → PR → done)

This is a **workflow FSM** (finite state machine) operating per WorkItem.

### 9.1 Workflow states (suggested)
- `New`
- `Planning`
- `Implementing`
- `PRDrafted`
- `AwaitingYourReview`
- `ReadyForReview`
- `InReview`
- `Done`
- `Abandoned` (explicit cancel)

### 9.2 Gates (where you get notified)
Configurable gates (default):
- Requirements ambiguity → NeedsDecision
- PR draft creation → no notification unless configured
- Convert draft to ready → NeedsApproval
- Merge → optionally NeedsApproval (many people keep this manual)

### 9.3 Automation triggers
Examples:
- When entering `Implementing`, orchestrator sends “begin implementation” instruction to OpenCode.
- When OpenCode emits `@@artifact` diff/commit or detects changes, orchestrator triggers:
  - run tests (either via OpenCode or via a “verification step”)
  - create draft PR via GitHub adapter
- When tests pass + PR exists, orchestrator creates `ReadyForReview` inbox item.

### 9.4 Linear state sync
Linear integration should:
- transition issue state when workflow transitions (configurable mapping)
- attach PR URL to the issue
- comment summaries (optional, can be noisy)

---

## 10) Terminal embedding (drop-in session view + backgrounding)

### 10.1 PTY runtime
You need a PTY manager supporting macOS + Linux:
- spawn process attached to PTY master/slave
- async read loop
- write via channel
- resize support

### 10.2 Terminal emulator
You must render ANSI control sequences properly (cursor moves, clears, alt screen, etc.). The UI displays a snapshot of the emulator state.

**Background behavior**
- Sessions continue running and output continues to be ingested.
- Rendering can be throttled when not visible.
- Store only a bounded scrollback (configurable) to avoid memory blowups.

### 10.3 Center view stack behavior
Center pane is a stack:

- InboxView (default)
- FocusCardView (on selecting inbox item)
- TerminalView(session) (push)
- Diff/Test/PR inspector views (push)

Key behavior:
- selecting inbox item **replaces** center view with FocusCardView
- “open terminal” **pushes** TerminalView
- “minimize” **pops** back to FocusCardView

---

## 11) TUI interaction model (vimlike + which-key)

### 11.1 Modes
- **Normal**: navigation + commands
- **Insert**: editing text (responses, chat input)

Hard rules:
- `Esc` always returns to Normal

### 11.2 Config-driven keymaps (trie)
Keymaps should be defined by config and compiled into a trie:
- mode-specific mapping
- prefix groups with labels (for which-key)
- commands are stable IDs in code; config binds keys → command ID + args

### 11.3 Which-key overlay
When the user types a prefix that is a valid partial command:
- overlay shows “next keys” and descriptions
- overlay disappears on completion, cancel, or invalid prefix

### 11.4 Command registry
All actions are commands:
- `ui.focus_next_inbox`
- `ui.open_terminal_for_selected`
- `supervisor.query { template: "status" }`
- `workflow.approve_pr_ready`
- `github.open_review_tabs`
- etc.

This makes keybinds and future scripting symmetrical.

---

## 12) Supervisor NL interface (OpenRouter streaming)

### 12.1 Why the supervisor exists
The supervisor answers questions *about the system state* and helps you respond quickly to prompts, without you opening terminals or reading logs.

### 12.2 Query engine (retrieval over EventStore)
Given a scope (selected inbox item, session, or “global”), it:
- pulls the relevant recent structured events
- pulls small evidence slices (diffstat, failing test tail, PR metadata)
- assembles a bounded context pack
- runs an OpenRouter streaming chat completion
- streams output into the UI incrementally

### 12.3 Streaming UX requirements
- incremental rendering into a “chat pane” widget
- cancellation keybind (`Ctrl-c` or `Esc`)
- backpressure: if tokens arrive faster than UI draw ticks, buffer and coalesce
- error surfaces: rate limit, auth, network failure → become inbox items or status line warnings

### 12.4 Templates + freeform
Support both:
- freeform: you type “why is this blocked?”
- template: keybind calls `supervisor.query(template=status_current_session)`

Templates should constrain output format:
- “Current activity”
- “What changed”
- “What needs me”
- “Risk assessment”
- “Recommended response”

---

## 13) Linear integration (1 session per ticket)

### 13.1 Mapping rules
- Each Linear issue is a WorkItem
- WorkItem owns one worktree and one harness session
- Orchestrator persists mapping so restart resumes

### 13.2 Sync strategy
- Poll or webhook later; start with polling:
_toggleable_:
  - fetch issues in relevant states (e.g., “In Progress”, “Todo” assigned to you)
  - update local cache
- For each issue chosen to start:
  - create worktree and spawn session if not already

### 13.3 Required operations
- list/search issues
- create issue (optional workflow)
- update issue state
- add comments/attachments (PR URL, summary)

---

## 14) Git + PR integration (adapters)

### 14.1 Git adapter
Operations:
- discover repo roots under configured workspace paths
- create worktree:
  - branch name template: `ap/{issue-key}-{slug}`
  - base branch: `main` or per-project config
- safe deletion:
  - default: never delete branch automatically
  - allow config: delete worktree dir after merge

### 14.2 GitHub adapter (via `gh`)
Operations:
- create draft PR (title/body templates, attach ticket link)
- mark ready for review
- request reviewers
- query “PRs waiting for my review”
- open URLs in browser (`open` on macOS, `xdg-open` on linux)

Keep provider behind a trait so GitLab/Bitbucket can be added later.

---

## 15) Persistence strategy

You want durability for:
- ticket ↔ worktree ↔ session mapping
- event history
- last known UI selection
- config last loaded hash (optional)

### 15.1 SQLite event store (recommended)
Tables (high-level):
- `projects`
- `tickets`
- `work_items`
- `worktrees`
- `sessions`
- `events` (append-only)
- `inbox_items`
- `artifacts`

Important:
- store large text blobs (logs, diffs) as artifacts with references
- keep event rows small; use artifact refs for big content

---

## 16) Extensibility plan (how to add “new tools” cleanly)

### 16.1 Adding a new harness (Codex, etc.)
Implement `WorkerBackend`:
- spawn spec
- PTY management can be shared by `orchestrator-runtime`
- event extraction:
  - if tool supports structured output, map to internal events
  - else enforce Supervisor Protocol tags via initial prompt injection

### 16.2 Adding a new LLM provider
Implement `LlmProvider`:
- streaming chat completions
- cancellation
- rate limit handling
- token accounting (optional)

### 16.3 Adding a new ticket system
Implement `TicketingProvider`:
- list issues
- update state
- create issue
- comment/attach

### 16.4 Adding a new git hosting provider
Implement `CodeHostProvider` (PR lifecycle, review queries)
- GitHub via `gh` is one backend
- Later: GitHub REST, GitLab, Bitbucket

### 16.5 Plugin boundary
Even if you don’t do dynamic plugins initially, treat each integration as a crate implementing a trait. That gives you “compile-time plugins” now and dynamic plugins later if you ever care.

---

## 17) Testing strategy

### 17.1 Unit tests
- keymap trie compilation + conflict detection
- attention engine scoring and batching (pure functions)
- workflow FSM transitions and guard conditions
- OpenRouter streaming parser and cancellation behavior
- protocol tag parser for harness output

### 17.2 Integration tests
- spawn a fake PTY child process that emits tags
- verify inbox items created correctly
- verify terminal view backgrounding and snapshot rendering

### 17.3 Golden tests (high value)
- feed a recorded event log into the UI state engine and assert rendered output snapshots

---

## 18) Security / safety rails (practical)
Even for “personal use,” you’ll want guardrails:
- never run destructive commands automatically (configurable)
- never force push
- never delete branches by default
- require explicit approval for:
  - converting PR from draft to ready (your gate)
  - anything resembling deployment

---

## 19) MVP build order (recommended)
If you want a usable system fast:

1) **EventStore + domain model** (enables everything else)
2) **PTY + terminal emulator + TerminalView** (proves embedding)
3) **OpenCode backend spawn + protocol parsing** (creates structured events)
4) **Attention engine + inbox/focus card UI** (core UX)
5) **Config keymaps + which-key overlay** (your muscle memory)
6) **Linear sync + mapping** (tie work to tickets)
7) **OpenRouter streaming supervisor** (NL & templates)
8) **Git worktrees + GitHub PR automation** (hands-off workflow)
9) Workflow FSM gates + end-to-end automation polish

---

## 20) Concrete implementation notes aligned to your current code

You already have:
- a composition root (`orchestrator-app`) that wires supervisor + github + ui.
- traits for dependencies (Supervisor, GithubClient).

Next evolutions to keep the same pattern:
- Expand `orchestrator-core` with **additional traits**:
  - WorkerBackend
  - TicketingProvider
  - VcsProvider / WorktreeManager
  - CodeHostProvider (PR lifecycle)
  - LlmProvider (OpenRouter streaming)
- Move the ratatui event loop into a richer UI state machine:
  - The UI should render from a `UiState` that is derived from domain state and current view stack.
- Keep the “process runner abstraction” you already used in `orchestrator-github`; reuse the same idea for:
  - git commands
  - open-browser commands
  - maybe opencode spawn (though that will be PTY-based)

---

## 21) What I would standardize early (to make this work smoothly)
If you do nothing else, do these two things early:

1) **Make the Supervisor Protocol real**  
   Even if OpenCode is brilliant, your orchestrator needs stable machine-readable signals to build focus cards and inbox items reliably.

2) **Keep the orchestrator dumb about code**  
   Orchestrator should route, gate, and present. OpenCode decides how to implement.

That combination produces a system that feels like “I only talk to the orchestrator,” without turning the orchestrator into a second full agent that fights OpenCode.
