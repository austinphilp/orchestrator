(() => {
  const lanes = ["decide", "approvals", "review", "fyi"];
  const laneLabels = {
    decide: "Decide / Unblock",
    approvals: "Approvals",
    review: "PR Reviews",
    fyi: "FYI Digest",
  };

  const defaultKeymapManifest = {
    modes: [
      {
        mode: "normal",
        bindings: [
          { keys: ["q"], command_id: "ui.shell.quit" },
          { keys: ["down"], command_id: "ui.focus_next_inbox" },
          { keys: ["j"], command_id: "ui.focus_next_inbox" },
          { keys: ["up"], command_id: "ui.focus_previous_inbox" },
          { keys: ["k"], command_id: "ui.focus_previous_inbox" },
          { keys: ["]"], command_id: "ui.cycle_batch_next" },
          { keys: ["["], command_id: "ui.cycle_batch_previous" },
          { keys: ["g"], command_id: "ui.jump_first_inbox" },
          { keys: ["G"], command_id: "ui.jump_last_inbox" },
          { keys: ["1"], command_id: "ui.jump_batch.decide_or_unblock" },
          { keys: ["2"], command_id: "ui.jump_batch.approvals" },
          { keys: ["3"], command_id: "ui.jump_batch.review_ready" },
          { keys: ["4"], command_id: "ui.jump_batch.fyi_digest" },
          { keys: ["s"], command_id: "ui.ticket_picker.open" },
          { keys: ["c"], command_id: "ui.supervisor_chat.toggle" },
          { keys: ["i"], command_id: "ui.mode.insert" },
          { keys: ["I"], command_id: "ui.open_terminal_for_selected" },
          { keys: ["o"], command_id: "ui.open_session_output_for_selected_inbox" },
          { keys: ["D"], command_id: "ui.worktree.diff.toggle" },
          { keys: ["w", "n"], command_id: "ui.terminal.workflow.advance" },
          { keys: ["x"], command_id: "ui.terminal.archive_selected_session" },
          { keys: ["z", "1"], command_id: "ui.jump_batch.decide_or_unblock" },
          { keys: ["z", "2"], command_id: "ui.jump_batch.approvals" },
          { keys: ["z", "3"], command_id: "ui.jump_batch.review_ready" },
          { keys: ["z", "4"], command_id: "ui.jump_batch.fyi_digest" },
          { keys: ["v", "d"], command_id: "ui.open_diff_inspector_for_selected" },
          { keys: ["v", "t"], command_id: "ui.open_test_inspector_for_selected" },
          { keys: ["v", "p"], command_id: "ui.open_pr_inspector_for_selected" },
          { keys: ["v", "c"], command_id: "ui.open_chat_inspector_for_selected" },
        ],
        prefixes: [
          { keys: ["z"], label: "Batch jumps" },
          { keys: ["v"], label: "Artifact inspectors" },
          { keys: ["w"], label: "Workflow Actions" },
        ],
      },
      { mode: "insert", bindings: [], prefixes: [] },
    ],
  };

  const $ = (id) => document.getElementById(id);
  const els = {
    statusLine: $("status-line"),
    connectionState: $("connection-state"),
    modeState: $("mode-state"),
    warningLine: $("warning-line"),
    hintLine: $("hint-line"),
    inboxList: $("inbox-list"),
    centerTitle: $("center-title"),
    centerBody: $("center-body"),
    sessionsList: $("sessions-list"),
    composeRow: $("compose-row"),
    composeInput: $("compose-input"),
    ticketPicker: $("ticket-picker"),
    ticketPickerBody: $("ticket-picker-body"),
    ticketPickerClose: $("ticket-picker-close"),
    diffModal: $("diff-modal"),
    diffContent: $("diff-content"),
    diffClose: $("diff-close"),
    archiveConfirm: $("archive-confirm"),
    archiveConfirmTitle: $("archive-confirm-title"),
    archiveConfirmMessage: $("archive-confirm-message"),
    archiveConfirmYes: $("archive-confirm-yes"),
    archiveConfirmNo: $("archive-confirm-no"),
    needsInputModal: $("needs-input-modal"),
    needsInputTitle: $("needs-input-title"),
    needsInputBody: $("needs-input-body"),
    needsInputFooter: $("needs-input-footer"),
    needsInputClose: $("needs-input-close"),
    toast: $("toast"),
  };

  const state = {
    connected: false,
    mode: "normal",
    snapshot: null,
    inboxRows: [],
    sessions: [],
    selectedInboxIndex: 0,
    activeSessionId: null,
    centerPanel: "terminal",
    warning: "ready",
    keyTrieByMode: { normal: createTrie(), insert: createTrie() },
    keySequence: [],
    keyHint: "",
    pendingRequests: new Map(),
    ws: null,
    wsRetryTimeout: null,
    requestSeq: 1,
    terminalBySession: {},
    ticketPicker: {
      visible: false,
      loading: false,
      tickets: [],
      projects: [],
      selectedIndex: 0,
      selectedProjectIndex: 0,
      newTicketMode: false,
      newTicketBrief: "",
      creating: false,
      error: null,
    },
    diffModal: {
      visible: false,
      loading: false,
      sessionId: null,
      content: "",
      error: null,
    },
    archiveConfirm: {
      visible: false,
      sessionId: null,
      action: null,
    },
    globalChat: {
      visible: false,
      input: "",
      streamId: null,
      query: "",
      response: "",
      status: "",
    },
    inspectorChat: {
      streamId: null,
      workItemId: null,
      response: "",
      status: "",
    },
  };

  function init() {
    initStaticListeners();
    void initializeData();
    document.addEventListener("keydown", handleKeyDown, { capture: true });
  }

  function initStaticListeners() {
    els.ticketPickerClose.addEventListener("click", () => {
      closeTicketPicker();
      render();
    });
    els.diffClose.addEventListener("click", () => {
      state.diffModal.visible = false;
      render();
    });
    els.archiveConfirmNo.addEventListener("click", () => {
      cancelArchiveConfirm();
      render();
    });
    els.archiveConfirmYes.addEventListener("click", () => {
      void confirmArchiveConfirm();
    });
    els.needsInputClose.addEventListener("click", () => {
      dismissNeedsInputComposer();
      render();
    });

    els.inboxList.addEventListener("click", (event) => {
      const target = event.target.closest("[data-inbox-index]");
      if (!target) return;
      const index = Number(target.getAttribute("data-inbox-index"));
      if (Number.isNaN(index)) return;
      selectInboxIndex(index);
      render();
    });

    els.sessionsList.addEventListener("click", (event) => {
      const target = event.target.closest("[data-session-id]");
      if (!target) return;
      const sessionId = target.getAttribute("data-session-id");
      if (!sessionId) return;
      state.activeSessionId = sessionId;
      state.centerPanel = "terminal";
      render();
    });

    els.ticketPickerBody.addEventListener("click", (event) => {
      const projectTarget = event.target.closest("[data-project-index]");
      if (projectTarget) {
        const projectIndex = Number(projectTarget.getAttribute("data-project-index"));
        if (!Number.isNaN(projectIndex)) {
          state.ticketPicker.selectedProjectIndex = projectIndex;
          render();
          return;
        }
      }

      const target = event.target.closest("[data-ticket-index]");
      if (!target) return;
      const index = Number(target.getAttribute("data-ticket-index"));
      if (Number.isNaN(index)) return;
      state.ticketPicker.selectedIndex = index;
      render();
    });
  }

  async function initializeData() {
    await loadKeymap();
    await refreshSnapshot();
    render();
    connectWebSocket();
  }

  async function loadKeymap() {
    const manifest = await fetchJson("/v1/keymap").catch(() => defaultKeymapManifest);
    state.keyTrieByMode = buildTries(manifest);
  }

  async function refreshSnapshot() {
    try {
      const snapshot = await fetchJson("/v1/snapshot");
      applySnapshot(snapshot);
    } catch (error) {
      state.warning = `snapshot unavailable: ${toMessage(error)}`;
    }
  }

  async function fetchJson(path) {
    const response = await fetch(path, { headers: { Accept: "application/json" } });
    if (!response.ok) {
      const body = await response.text();
      throw new Error(`HTTP ${response.status}: ${body}`);
    }
    return response.json();
  }

  function connectWebSocket() {
    clearTimeout(state.wsRetryTimeout);
    const socket = new WebSocket(wsUrl("/v1/ws"));
    state.ws = socket;

    socket.addEventListener("open", () => {
      state.connected = true;
      state.warning = "connected";
      sendEnvelope({ type: "session.init", payload: {} });
      render();
    });

    socket.addEventListener("message", (event) => {
      handleSocketMessage(event.data);
    });

    socket.addEventListener("close", () => {
      state.connected = false;
      rejectAllPending("connection closed");
      render();
      state.wsRetryTimeout = setTimeout(connectWebSocket, 1200);
    });

    socket.addEventListener("error", () => {
      state.connected = false;
      render();
    });
  }

  function wsUrl(path) {
    const protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
    return `${protocol}//${window.location.host}${path}`;
  }

  function handleSocketMessage(raw) {
    let envelope;
    try {
      envelope = JSON.parse(raw);
    } catch {
      return;
    }

    if (envelope.type === "response.ok" || envelope.type === "response.error") {
      const pending = state.pendingRequests.get(envelope.request_id);
      if (!pending) return;
      state.pendingRequests.delete(envelope.request_id);
      if (envelope.type === "response.ok") {
        pending.resolve(envelope.payload);
      } else {
        pending.reject(new Error(envelope.payload?.message || "request failed"));
      }
      return;
    }

    handleEvent(envelope.type, envelope.payload || {});
  }

  function handleEvent(type, payload) {
    switch (type) {
      case "frontend.snapshot.updated":
        applySnapshot(payload);
        break;
      case "frontend.notification":
        state.warning = formatNotification(payload);
        showToast(state.warning);
        break;
      case "terminal.output":
        onTerminalOutput(payload);
        break;
      case "terminal.turn_state":
        onTerminalTurnState(payload);
        break;
      case "terminal.needs_input":
        onTerminalNeedsInput(payload);
        break;
      case "terminal.stream_failed":
        onTerminalStreamFailed(payload);
        break;
      case "terminal.stream_ended":
        onTerminalStreamEnded(payload);
        break;
      case "supervisor.started":
        onSupervisorStarted(payload);
        break;
      case "supervisor.delta":
        onSupervisorDelta(payload);
        break;
      case "supervisor.finished":
        onSupervisorFinished(payload);
        break;
      case "supervisor.failed":
        onSupervisorFailed(payload);
        break;
      case "merge.queue.event":
        onMergeQueueEvent(payload);
        break;
      case "error":
        state.warning = payload.message || "ws error";
        showToast(state.warning);
        break;
      default:
        break;
    }
    render();
  }

  function onTerminalOutput(payload) {
    const sessionId = payload.session_id;
    if (!sessionId || !payload.output) return;
    const entry = ensureTerminalSession(sessionId);
    const bytes = Array.isArray(payload.output.bytes) ? payload.output.bytes : [];
    const text = decodeBytes(bytes);
    if (!text) return;

    if (payload.output.stream === "Stderr") {
      entry.output += `\n[stderr]\n${text}`;
    } else {
      entry.output += text;
    }

    if (entry.output.length > 250000) {
      entry.output = entry.output.slice(entry.output.length - 250000);
    }
  }

  function onTerminalTurnState(payload) {
    const sessionId = payload.session_id;
    if (!sessionId) return;
    const entry = ensureTerminalSession(sessionId);
    const turnState = payload.turn_state || {};
    entry.turnActive = Boolean(turnState.active);
  }

  function onTerminalNeedsInput(payload) {
    const sessionId = payload.session_id;
    if (!sessionId) return;
    const entry = ensureTerminalSession(sessionId);
    const nextNeedsInput = payload.needs_input || null;
    entry.needsInput = nextNeedsInput;
    if (!nextNeedsInput) {
      entry.needsInputComposer = null;
      return;
    }

    const nextPromptId = String(nextNeedsInput.prompt_id || "");
    const existingPromptId = entry.needsInputComposer?.promptId || null;
    if (!entry.needsInputComposer || existingPromptId !== nextPromptId) {
      entry.needsInputComposer = createNeedsInputComposer(nextNeedsInput);
    }
    state.centerPanel = "terminal";
    state.activeSessionId = sessionId;
  }

  function onTerminalStreamFailed(payload) {
    const sessionId = payload.session_id;
    if (!sessionId) return;
    const entry = ensureTerminalSession(sessionId);
    entry.error = payload.message || "stream failed";
    entry.turnActive = false;
    entry.needsInput = null;
    entry.needsInputComposer = null;
  }

  function onTerminalStreamEnded(payload) {
    const sessionId = payload.session_id;
    if (!sessionId) return;
    const entry = ensureTerminalSession(sessionId);
    entry.turnActive = false;
    entry.needsInput = null;
    entry.needsInputComposer = null;
  }

  function onSupervisorStarted(payload) {
    const streamId = payload.stream_id;
    if (!streamId) return;

    if (state.globalChat.streamId === streamId) {
      state.globalChat.status = "streaming";
      return;
    }

    if (state.inspectorChat.streamId === streamId) {
      state.inspectorChat.status = "streaming";
    }
  }

  function onSupervisorDelta(payload) {
    const streamId = payload.stream_id;
    const text = payload.text || "";
    if (!streamId || !text) return;

    if (state.globalChat.streamId === streamId) {
      state.globalChat.response += text;
      return;
    }

    if (state.inspectorChat.streamId === streamId) {
      state.inspectorChat.response += text;
    }
  }

  function onSupervisorFinished(payload) {
    const streamId = payload.stream_id;
    if (!streamId) return;

    if (state.globalChat.streamId === streamId) {
      state.globalChat.status = `finished (${payload.reason || "stop"})`;
      return;
    }
    if (state.inspectorChat.streamId === streamId) {
      state.inspectorChat.status = `finished (${payload.reason || "stop"})`;
    }
  }

  function onSupervisorFailed(payload) {
    const streamId = payload.stream_id;
    if (!streamId) return;
    const message = payload.message || "supervisor stream failed";

    if (state.globalChat.streamId === streamId) {
      state.globalChat.status = "failed";
      state.globalChat.response += `\n\n[error] ${message}`;
      showToast(message);
      return;
    }
    if (state.inspectorChat.streamId === streamId) {
      state.inspectorChat.status = "failed";
      state.inspectorChat.response += `\n\n[error] ${message}`;
      showToast(message);
    }
  }

  function onMergeQueueEvent(payload) {
    if (!payload || !payload.kind) return;
    if (payload.kind === "completed") {
      const suffix = payload.completed ? "completed" : "pending";
      state.warning = `merge ${payload.command_kind}: ${suffix}`;
    } else if (payload.kind === "session_finalize_failed") {
      state.warning = payload.message || "session finalize failed";
    } else if (payload.kind === "session_finalized") {
      state.warning = "merge finalized";
    }
  }

  function applySnapshot(snapshot) {
    state.snapshot = snapshot;
    state.inboxRows = deriveInboxRows(snapshot);
    state.sessions = deriveSessions(snapshot);

    if (state.inboxRows.length === 0) {
      state.selectedInboxIndex = 0;
    } else if (state.selectedInboxIndex >= state.inboxRows.length) {
      state.selectedInboxIndex = state.inboxRows.length - 1;
    }

    const selectedRow = getSelectedInboxRow();
    if (selectedRow?.session_id) {
      state.activeSessionId = selectedRow.session_id;
    } else if (state.sessions.length > 0 && !sessionExists(state.activeSessionId)) {
      state.activeSessionId = state.sessions[0].id;
    }

    state.statusText = snapshot?.status || "ready";
  }

  function deriveInboxRows(snapshot) {
    const projection = snapshot?.projection || {};
    const inboxItems = projection.inbox_items || {};
    const workItems = projection.work_items || {};
    const sessions = projection.sessions || {};

    const rows = [];
    for (const item of Object.values(inboxItems)) {
      if (!item || item.resolved) continue;
      const workItem = workItems[item.work_item_id];
      const sessionId = workItem?.session_id || null;
      const session = sessionId ? sessions[sessionId] : null;

      rows.push({
        id: item.id,
        title: item.title,
        kind: item.kind,
        lane: laneForItemKind(item.kind),
        work_item_id: item.work_item_id,
        session_id: sessionId,
        workflow_state: workItem?.workflow_state || null,
        session_status: session?.status || null,
      });
    }

    rows.sort((a, b) => {
      const laneCmp = lanes.indexOf(a.lane) - lanes.indexOf(b.lane);
      if (laneCmp !== 0) return laneCmp;
      return String(a.title).localeCompare(String(b.title));
    });

    return rows;
  }

  function deriveSessions(snapshot) {
    const sessions = snapshot?.projection?.sessions || {};
    const items = Object.values(sessions);
    items.sort((a, b) => {
      const aRank = sessionStatusRank(a.status);
      const bRank = sessionStatusRank(b.status);
      if (aRank !== bRank) return aRank - bRank;
      return String(a.id).localeCompare(String(b.id));
    });
    return items;
  }

  function laneForItemKind(kind) {
    switch (kind) {
      case "NeedsDecision":
      case "Blocked":
        return "decide";
      case "NeedsApproval":
        return "approvals";
      case "ReadyForReview":
        return "review";
      case "FYI":
      default:
        return "fyi";
    }
  }

  function sessionStatusRank(status) {
    switch (status) {
      case "Running":
        return 0;
      case "WaitingForUser":
        return 1;
      case "Blocked":
        return 2;
      case "Done":
        return 3;
      case "Crashed":
        return 4;
      default:
        return 5;
    }
  }

  function getSelectedInboxRow() {
    return state.inboxRows[state.selectedInboxIndex] || null;
  }

  function sessionExists(sessionId) {
    if (!sessionId) return false;
    return state.sessions.some((session) => session.id === sessionId);
  }

  function ensureTerminalSession(sessionId) {
    if (!state.terminalBySession[sessionId]) {
      state.terminalBySession[sessionId] = {
        output: "",
        turnActive: false,
        needsInput: null,
        needsInputComposer: null,
        error: null,
      };
    }
    return state.terminalBySession[sessionId];
  }

  function activeTerminalEntry() {
    const sessionId = selectedSessionId();
    if (!sessionId) return null;
    return ensureTerminalSession(sessionId);
  }

  function activeNeedsInputComposer() {
    const entry = activeTerminalEntry();
    if (!entry) return null;
    return entry.needsInputComposer || null;
  }

  function normalizeNeedsInputQuestions(needsInput) {
    const structuredQuestions = Array.isArray(needsInput?.questions) ? needsInput.questions : [];
    if (structuredQuestions.length > 0) {
      return structuredQuestions.map((question, index) => {
        const options = Array.isArray(question.options) ? question.options : [];
        const normalizedOptions = options
          .map((option) => {
            if (typeof option === "string") {
              return { label: option, description: "" };
            }
            return {
              label: String(option?.label || ""),
              description: String(option?.description || ""),
            };
          })
          .filter((option) => option.label.trim().length > 0);
        return {
          id: String(question?.id || `q-${index + 1}`),
          header: String(question?.header || "Input"),
          question: String(question?.question || ""),
          is_other: Boolean(question?.is_other),
          is_secret: Boolean(question?.is_secret),
          options: normalizedOptions,
        };
      });
    }

    const fallbackOptions = Array.isArray(needsInput?.options) ? needsInput.options : [];
    return [
      {
        id: String(needsInput?.prompt_id || "input"),
        header: "Input",
        question: String(needsInput?.question || ""),
        is_other: false,
        is_secret: false,
        options: fallbackOptions
          .map((option) => String(option || "").trim())
          .filter((option) => option.length > 0)
          .map((label) => ({ label, description: "" })),
      },
    ];
  }

  function recommendedOptionIndex(options) {
    if (!Array.isArray(options)) return null;
    const index = options.findIndex((option) =>
      String(option?.label || "")
        .toLowerCase()
        .includes("(recommended)")
    );
    return index >= 0 ? index : null;
  }

  function createNeedsInputComposer(needsInput) {
    const questions = normalizeNeedsInputQuestions(needsInput);
    const defaultOptionLabel =
      typeof needsInput?.default_option === "string" ? needsInput.default_option : null;
    const drafts = questions.map((question, index) => {
      const options = Array.isArray(question.options) ? question.options : [];
      let selectedOptionIndex = null;
      if (options.length > 0) {
        if (index === 0 && defaultOptionLabel) {
          const defaultIndex = options.findIndex((option) => option.label === defaultOptionLabel);
          if (defaultIndex >= 0) {
            selectedOptionIndex = defaultIndex;
          }
        }
        if (selectedOptionIndex === null) {
          selectedOptionIndex = recommendedOptionIndex(options);
        }
      }
      const optionCursor = selectedOptionIndex === null ? 0 : selectedOptionIndex;
      return {
        selectedOptionIndex,
        optionCursor,
        note: "",
      };
    });

    return {
      promptId: String(needsInput?.prompt_id || `prompt-${Date.now()}`),
      questions,
      drafts,
      currentQuestionIndex: 0,
      noteMode: false,
      error: null,
    };
  }

  function currentNeedsInputDraft(composer) {
    if (!composer) return null;
    const index = composer.currentQuestionIndex;
    return composer.drafts[index] || null;
  }

  function dismissNeedsInputComposer() {
    const entry = activeTerminalEntry();
    if (!entry) return;
    entry.needsInputComposer = null;
    entry.needsInput = null;
    state.mode = "normal";
  }

  function moveNeedsInputQuestion(delta) {
    const composer = activeNeedsInputComposer();
    if (!composer) return;
    const total = composer.questions.length;
    if (total === 0) return;
    const next = Math.max(0, Math.min(total - 1, composer.currentQuestionIndex + delta));
    composer.currentQuestionIndex = next;
    composer.noteMode = false;
    composer.error = null;
  }

  function moveNeedsInputOptionCursor(delta) {
    const composer = activeNeedsInputComposer();
    if (!composer) return;
    const question = composer.questions[composer.currentQuestionIndex];
    if (!question) return;
    const options = Array.isArray(question.options) ? question.options : [];
    if (options.length === 0) return;
    const draft = currentNeedsInputDraft(composer);
    if (!draft) return;
    const cursor = Number.isFinite(draft.optionCursor) ? draft.optionCursor : 0;
    draft.optionCursor = (cursor + delta + options.length) % options.length;
  }

  function chooseNeedsInputHighlightedOption() {
    const composer = activeNeedsInputComposer();
    if (!composer) return false;
    const question = composer.questions[composer.currentQuestionIndex];
    if (!question) return false;
    const options = Array.isArray(question.options) ? question.options : [];
    if (options.length === 0) return false;
    const draft = currentNeedsInputDraft(composer);
    if (!draft) return false;
    let cursor = Number.isFinite(draft.optionCursor) ? draft.optionCursor : 0;
    cursor = Math.max(0, Math.min(options.length - 1, cursor));
    draft.optionCursor = cursor;
    draft.selectedOptionIndex = cursor;
    composer.error = null;
    return true;
  }

  function toggleNeedsInputNoteMode(enabled) {
    const composer = activeNeedsInputComposer();
    if (!composer) return;
    composer.noteMode = enabled;
    state.mode = enabled ? "insert" : "normal";
  }

  function buildNeedsInputAnswers(composer) {
    const answers = [];
    for (let index = 0; index < composer.questions.length; index += 1) {
      const question = composer.questions[index];
      const draft = composer.drafts[index] || { selectedOptionIndex: null, note: "" };
      const response = [];
      const options = Array.isArray(question.options) ? question.options : [];
      if (options.length > 0) {
        const selected = draft.selectedOptionIndex;
        if (selected === null || selected < 0 || selected >= options.length) {
          return {
            answers: null,
            error: `Select an option for '${question.header || question.question || question.id}'.`,
          };
        }
        response.push(String(options[selected].label || ""));
      }

      const note = String(draft.note || "").trim();
      if (note.length > 0) {
        response.push(note);
      }
      if (response.length === 0) {
        return {
          answers: null,
          error: `Provide a response for '${question.header || question.question || question.id}'.`,
        };
      }
      answers.push({
        question_id: String(question.id || `q-${index + 1}`),
        answers: response,
      });
    }

    return { answers, error: null };
  }

  async function submitNeedsInputComposer() {
    const sessionId = selectedSessionId();
    const entry = activeTerminalEntry();
    const composer = activeNeedsInputComposer();
    if (!sessionId || !entry || !composer) return;
    const { answers, error } = buildNeedsInputAnswers(composer);
    if (error) {
      composer.error = error;
      return;
    }

    try {
      await wsRequest("frontend.intent.needs_input_response", {
        session_id: sessionId,
        prompt_id: composer.promptId,
        answers,
      });
      entry.needsInput = null;
      entry.needsInputComposer = null;
      state.warning = `needs input submitted for ${sessionId}`;
      state.mode = "normal";
    } catch (requestError) {
      composer.error = toMessage(requestError);
      showToast(composer.error);
    }
  }

  function decodeBytes(bytes) {
    if (!Array.isArray(bytes) || bytes.length === 0) return "";
    const array = Uint8Array.from(bytes);
    try {
      return new TextDecoder().decode(array);
    } catch {
      return "";
    }
  }

  function buildTries(manifest) {
    const byMode = { normal: createTrie(), insert: createTrie() };
    const modes = Array.isArray(manifest?.modes) ? manifest.modes : [];
    for (const mode of modes) {
      const modeName = mode.mode === "insert" ? "insert" : "normal";
      const root = createTrie();

      for (const binding of mode.bindings || []) {
        insertTrie(root, binding.keys || [], binding.command_id || null, null);
      }

      for (const prefix of mode.prefixes || []) {
        insertTrie(root, prefix.keys || [], null, prefix.label || "prefix");
      }

      byMode[modeName] = root;
    }
    return byMode;
  }

  function createTrie() {
    return { command: null, label: null, children: Object.create(null) };
  }

  function insertTrie(root, keys, commandId, label) {
    if (!Array.isArray(keys) || keys.length === 0) return;
    let node = root;
    for (const key of keys) {
      if (!node.children[key]) {
        node.children[key] = createTrie();
      }
      node = node.children[key];
    }
    if (commandId) node.command = commandId;
    if (label) node.label = label;
  }

  function routeNormalKey(token) {
    const root = state.keyTrieByMode.normal || createTrie();
    const hadPendingPrefix = state.keySequence.length > 0;
    state.keySequence.push(token);

    let node = root;
    for (const part of state.keySequence) {
      node = node.children[part];
      if (!node) {
        if (hadPendingPrefix) {
          const fallback = root.children[token];
          state.keySequence = [];
          state.keyHint = "";
          if (fallback?.command) {
            return fallback.command;
          }
          return null;
        }

        state.keySequence = [];
        state.keyHint = "";
        return null;
      }
    }

    state.keyHint = node.label || "";

    if (node.command) {
      const command = node.command;
      state.keySequence = [];
      state.keyHint = "";
      return command;
    }

    return null;
  }

  async function handleKeyDown(event) {
    if (event.target && ["INPUT", "TEXTAREA"].includes(event.target.tagName)) {
      event.preventDefault();
    }

    if (state.diffModal.visible) {
      const handled = handleDiffModalKey(event);
      if (handled) {
        event.preventDefault();
        render();
      }
      return;
    }

    if (state.ticketPicker.visible) {
      const handled = await handleTicketPickerKey(event);
      if (handled) {
        event.preventDefault();
        render();
      }
      return;
    }

    if (state.archiveConfirm.visible) {
      const handled = await handleArchiveConfirmKey(event);
      if (handled) {
        event.preventDefault();
        render();
      }
      return;
    }

    if (activeNeedsInputComposer() && state.centerPanel === "terminal") {
      const handled = await handleNeedsInputKey(event);
      if (handled) {
        event.preventDefault();
        render();
      }
      return;
    }

    if (event.key === "Escape") {
      event.preventDefault();
      state.mode = "normal";
      state.keySequence = [];
      state.keyHint = "";
      if (state.globalChat.visible) {
        state.globalChat.visible = false;
      }
      render();
      return;
    }

    if (state.mode === "insert") {
      const handled = await handleInsertModeKey(event);
      if (handled) {
        event.preventDefault();
        render();
      }
      return;
    }

    const token = keyToken(event);
    if (!token) return;

    const commandId = routeNormalKey(token);
    event.preventDefault();

    if (!commandId) {
      render();
      return;
    }

    await executeCommand(commandId);
    render();
  }

  function handleDiffModalKey(event) {
    if (event.ctrlKey || event.metaKey || event.altKey) {
      return true;
    }

    const token = keyToken(event);
    if (event.key === "Escape" || token === "q" || token === "D") {
      state.diffModal.visible = false;
      return true;
    }

    if (token === "j" || token === "down") {
      els.diffContent.scrollTop += 80;
      return true;
    }
    if (token === "k" || token === "up") {
      els.diffContent.scrollTop -= 80;
      return true;
    }
    if (token === "pagedown") {
      els.diffContent.scrollTop += 360;
      return true;
    }
    if (token === "pageup") {
      els.diffContent.scrollTop -= 360;
      return true;
    }
    if (token === "g") {
      els.diffContent.scrollTop = 0;
      return true;
    }
    if (token === "G") {
      els.diffContent.scrollTop = els.diffContent.scrollHeight;
      return true;
    }
    return true;
  }

  async function handleTicketPickerKey(event) {
    if (state.ticketPicker.newTicketMode) {
      return handleTicketPickerCreateKey(event);
    }

    if (event.key === "Escape") {
      closeTicketPicker();
      return true;
    }
    if (event.ctrlKey || event.metaKey || event.altKey) {
      return true;
    }

    const token = keyToken(event);
    switch (token) {
      case "j":
      case "down":
        moveTicketPickerSelection(1);
        return true;
      case "k":
      case "up":
        moveTicketPickerSelection(-1);
        return true;
      case "enter":
        await startSelectedTicketFromPicker();
        return true;
      case "x":
        await archiveSelectedTicketFromPicker();
        return true;
      case "n":
        beginCreateTicketFromPicker();
        return true;
      default:
        return true;
    }
  }

  async function handleTicketPickerCreateKey(event) {
    if (event.key === "Escape") {
      cancelCreateTicketFromPicker();
      return true;
    }
    if (event.ctrlKey || event.metaKey || event.altKey) {
      return true;
    }

    if (event.key === "Enter" && event.shiftKey) {
      await submitCreateTicketFromPicker("create_and_start");
      return true;
    }
    if (event.key === "Enter") {
      await submitCreateTicketFromPicker("create_only");
      return true;
    }
    if (event.key === "Backspace") {
      state.ticketPicker.newTicketBrief = state.ticketPicker.newTicketBrief.slice(0, -1);
      return true;
    }

    const token = keyToken(event);
    if (token === "up" || token === "k") {
      moveTicketPickerProjectSelection(-1);
      return true;
    }
    if (token === "down" || token === "j" || token === "tab") {
      moveTicketPickerProjectSelection(1);
      return true;
    }
    if (token === "backtab") {
      moveTicketPickerProjectSelection(-1);
      return true;
    }

    if (event.key.length === 1) {
      state.ticketPicker.newTicketBrief += event.key;
      return true;
    }
    return true;
  }

  async function handleArchiveConfirmKey(event) {
    if (event.key === "Escape") {
      cancelArchiveConfirm();
      return true;
    }
    if (event.ctrlKey || event.metaKey || event.altKey) {
      return true;
    }
    const token = keyToken(event);
    if (token === "n") {
      cancelArchiveConfirm();
      return true;
    }
    if (token === "y" || token === "enter") {
      await confirmArchiveConfirm();
      return true;
    }
    return true;
  }

  async function handleNeedsInputKey(event) {
    const composer = activeNeedsInputComposer();
    if (!composer) return false;

    if (composer.noteMode) {
      return handleNeedsInputNoteInsertKey(event);
    }

    if (event.key === "Escape") {
      state.mode = "normal";
      return true;
    }
    if (event.ctrlKey || event.metaKey || event.altKey) {
      return true;
    }

    const token = keyToken(event);
    switch (token) {
      case "i":
        toggleNeedsInputNoteMode(true);
        return true;
      case "left":
      case "h":
        moveNeedsInputQuestion(-1);
        return true;
      case "right":
      case "l":
        moveNeedsInputQuestion(1);
        return true;
      case "up":
      case "k":
        moveNeedsInputOptionCursor(-1);
        return true;
      case "down":
      case "j":
        moveNeedsInputOptionCursor(1);
        return true;
      case "home": {
        const draft = currentNeedsInputDraft(composer);
        if (draft) draft.optionCursor = 0;
        return true;
      }
      case "end": {
        const question = composer.questions[composer.currentQuestionIndex];
        const options = Array.isArray(question?.options) ? question.options : [];
        const draft = currentNeedsInputDraft(composer);
        if (draft && options.length > 0) {
          draft.optionCursor = options.length - 1;
        }
        return true;
      }
      case "pageup":
        moveNeedsInputOptionCursor(-5);
        return true;
      case "pagedown":
        moveNeedsInputOptionCursor(5);
        return true;
      case " ":
        chooseNeedsInputHighlightedOption();
        return true;
      case "enter": {
        const question = composer.questions[composer.currentQuestionIndex];
        const options = Array.isArray(question?.options) ? question.options : [];
        const draft = currentNeedsInputDraft(composer);
        if (options.length > 0 && draft && draft.selectedOptionIndex === null) {
          chooseNeedsInputHighlightedOption();
        }
        const lastQuestion = composer.currentQuestionIndex + 1 >= composer.questions.length;
        if (lastQuestion) {
          await submitNeedsInputComposer();
        } else {
          moveNeedsInputQuestion(1);
        }
        return true;
      }
      default:
        return true;
    }
  }

  function handleNeedsInputNoteInsertKey(event) {
    const composer = activeNeedsInputComposer();
    const draft = currentNeedsInputDraft(composer);
    if (!composer || !draft) return false;
    if (event.ctrlKey || event.metaKey || event.altKey) {
      return true;
    }

    if (event.key === "Escape") {
      toggleNeedsInputNoteMode(false);
      return true;
    }
    if (event.key === "Enter" && event.shiftKey) {
      draft.note += "\n";
      return true;
    }
    if (event.key === "Enter") {
      toggleNeedsInputNoteMode(false);
      return true;
    }
    if (event.key === "Backspace") {
      draft.note = draft.note.slice(0, -1);
      return true;
    }
    if (event.key.length === 1) {
      draft.note += event.key;
      return true;
    }
    return true;
  }

  async function handleInsertModeKey(event) {
    if (event.ctrlKey || event.metaKey || event.altKey) {
      return false;
    }

    if (state.globalChat.visible) {
      if (event.key === "Enter") {
        await submitGlobalChatQuery();
        return true;
      }
      if (event.key === "Backspace") {
        state.globalChat.input = state.globalChat.input.slice(0, -1);
        return true;
      }
      if (event.key.length === 1) {
        state.globalChat.input += event.key;
        return true;
      }
      return false;
    }

    if (state.centerPanel !== "terminal") {
      return false;
    }

    const sessionId = selectedSessionId();
    if (!sessionId) return false;

    if (event.key === "Enter") {
      await submitTerminalInput(sessionId);
      return true;
    }

    if (event.key === "Backspace") {
      terminalCompose.input = terminalCompose.input.slice(0, -1);
      return true;
    }

    if (event.key.length === 1) {
      terminalCompose.input += event.key;
      return true;
    }

    return false;
  }

  const terminalCompose = { input: "" };

  async function submitTerminalInput(sessionId) {
    const text = terminalCompose.input;
    if (!text.trim()) {
      state.mode = "normal";
      return;
    }
    terminalCompose.input = "";
    await wsRequest("frontend.intent.terminal_input", { session_id: sessionId, input: text }).catch(
      (error) => {
        state.warning = toMessage(error);
        showToast(state.warning);
      }
    );
    state.mode = "normal";
  }

  async function submitGlobalChatQuery() {
    const query = state.globalChat.input.trim();
    if (!query) {
      state.warning = "enter a non-empty supervisor query";
      return;
    }

    state.globalChat.input = "";
    state.globalChat.query = query;
    state.globalChat.response = "";
    state.globalChat.status = "starting";

    try {
      const result = await wsRequest("supervisor.query.start", {
        invocation: {
          command_id: "supervisor.query",
          args: {
            kind: "freeform",
            query,
            context: {
              scope: "global",
            },
          },
        },
        context: {
          scope: "global",
        },
      });
      state.globalChat.streamId = result.stream_id || null;
    } catch (error) {
      state.globalChat.status = "failed";
      state.globalChat.response = `[error] ${toMessage(error)}`;
    }
  }

  async function executeCommand(commandId) {
    switch (commandId) {
      case "ui.mode.normal":
        state.mode = "normal";
        return;
      case "ui.mode.insert":
        state.mode = "insert";
        return;
      case "ui.supervisor_chat.toggle":
        state.globalChat.visible = !state.globalChat.visible;
        if (state.globalChat.visible) {
          state.centerPanel = "chat";
        }
        return;
      case "ui.open_terminal_for_selected":
        state.centerPanel = "terminal";
        if (getSelectedInboxRow()?.session_id) {
          state.activeSessionId = getSelectedInboxRow().session_id;
        }
        return;
      case "ui.open_session_output_for_selected_inbox":
        await openSelectedInboxSessionOutput();
        return;
      case "ui.open_diff_inspector_for_selected":
        state.centerPanel = "diff";
        return;
      case "ui.open_test_inspector_for_selected":
        state.centerPanel = "tests";
        return;
      case "ui.open_pr_inspector_for_selected":
        state.centerPanel = "pr";
        return;
      case "ui.open_chat_inspector_for_selected":
        state.centerPanel = "chat";
        state.globalChat.visible = false;
        await startInspectorChatStream();
        return;
      case "ui.focus_next_inbox":
        moveSelection(1);
        return;
      case "ui.focus_previous_inbox":
        moveSelection(-1);
        return;
      case "ui.cycle_batch_next":
        cycleLane(1);
        return;
      case "ui.cycle_batch_previous":
        cycleLane(-1);
        return;
      case "ui.jump_first_inbox":
        selectInboxIndex(0);
        return;
      case "ui.jump_last_inbox":
        selectInboxIndex(state.inboxRows.length - 1);
        return;
      case "ui.jump_batch.decide_or_unblock":
        jumpToLane("decide");
        return;
      case "ui.jump_batch.approvals":
        jumpToLane("approvals");
        return;
      case "ui.jump_batch.review_ready":
        jumpToLane("review");
        return;
      case "ui.jump_batch.fyi_digest":
        jumpToLane("fyi");
        return;
      case "ui.ticket_picker.open":
        await openTicketPicker();
        return;
      case "ui.ticket_picker.close":
        closeTicketPicker();
        return;
      case "ui.ticket_picker.move_next":
        moveTicketPickerSelection(1);
        return;
      case "ui.ticket_picker.move_previous":
        moveTicketPickerSelection(-1);
        return;
      case "ui.ticket_picker.fold_project":
      case "ui.ticket_picker.unfold_project":
        return;
      case "ui.ticket_picker.start_selected":
        await startSelectedTicketFromPicker();
        return;
      case "ui.worktree.diff.toggle":
        await toggleDiffModal();
        return;
      case "ui.terminal.workflow.advance":
        await advanceSelectedSessionWorkflow();
        return;
      case "ui.terminal.archive_selected_session":
        beginArchiveConfirm();
        return;
      case "ui.shell.quit":
        showToast("Web UI cannot quit browser tab via keybind.");
        return;
      default:
        await wsRequest("frontend.intent.command", { command_id: commandId }).catch((error) => {
          state.warning = toMessage(error);
          showToast(state.warning);
        });
    }
  }

  function moveSelection(delta) {
    if (state.inboxRows.length === 0) return;
    let next = state.selectedInboxIndex + delta;
    if (next < 0) next = 0;
    if (next >= state.inboxRows.length) next = state.inboxRows.length - 1;
    selectInboxIndex(next);
  }

  function selectInboxIndex(index) {
    if (!Number.isFinite(index)) return;
    if (state.inboxRows.length === 0) {
      state.selectedInboxIndex = 0;
      return;
    }
    const clamped = Math.max(0, Math.min(index, state.inboxRows.length - 1));
    state.selectedInboxIndex = clamped;
    const row = state.inboxRows[clamped];
    if (row?.session_id) {
      state.activeSessionId = row.session_id;
    }
  }

  function cycleLane(direction) {
    if (state.inboxRows.length === 0) return;
    const currentLane = getSelectedInboxRow()?.lane || lanes[0];
    const currentIndex = lanes.indexOf(currentLane);
    const nextIndex = (currentIndex + direction + lanes.length) % lanes.length;
    jumpToLane(lanes[nextIndex]);
  }

  function jumpToLane(lane) {
    const index = state.inboxRows.findIndex((row) => row.lane === lane);
    if (index >= 0) {
      selectInboxIndex(index);
    }
  }

  async function openSelectedInboxSessionOutput() {
    const row = getSelectedInboxRow();
    if (!row) {
      showToast("Select an inbox item first.");
      return;
    }
    if (!row.session_id) {
      showToast("Selected inbox item has no active session.");
      return;
    }

    state.centerPanel = "terminal";
    state.activeSessionId = row.session_id;
    try {
      await wsRequest("inbox.resolve", {
        inbox_item_id: row.id,
        work_item_id: row.work_item_id,
      });
      await refreshSnapshot();
    } catch (error) {
      showToast(toMessage(error));
    }
  }

  async function openTicketPicker() {
    state.ticketPicker.visible = true;
    state.ticketPicker.loading = true;
    state.ticketPicker.creating = false;
    state.ticketPicker.newTicketMode = false;
    state.ticketPicker.newTicketBrief = "";
    state.ticketPicker.error = null;

    try {
      const [ticketResult, projectResult] = await Promise.all([
        wsRequest("ticket.list", {}),
        wsRequest("ticket.projects.list", {}).catch(() => ({ projects: [] })),
      ]);
      state.ticketPicker.tickets = ticketResult.tickets || [];
      state.ticketPicker.projects = Array.isArray(projectResult.projects) ? projectResult.projects : [];
      state.ticketPicker.selectedIndex = 0;
      state.ticketPicker.selectedProjectIndex = 0;
    } catch (error) {
      state.ticketPicker.error = toMessage(error);
    } finally {
      state.ticketPicker.loading = false;
    }
  }

  function closeTicketPicker() {
    state.ticketPicker.visible = false;
    state.ticketPicker.loading = false;
    state.ticketPicker.newTicketMode = false;
    state.ticketPicker.newTicketBrief = "";
    state.ticketPicker.creating = false;
  }

  function moveTicketPickerSelection(delta) {
    if (!state.ticketPicker.visible || state.ticketPicker.tickets.length === 0) return;
    let next = state.ticketPicker.selectedIndex + delta;
    next = Math.max(0, Math.min(next, state.ticketPicker.tickets.length - 1));
    state.ticketPicker.selectedIndex = next;
  }

  function beginCreateTicketFromPicker() {
    if (!state.ticketPicker.visible || state.ticketPicker.loading || state.ticketPicker.creating) {
      return;
    }
    state.ticketPicker.newTicketMode = true;
    state.ticketPicker.error = null;
  }

  function cancelCreateTicketFromPicker() {
    state.ticketPicker.newTicketMode = false;
    state.ticketPicker.newTicketBrief = "";
    state.ticketPicker.creating = false;
    state.ticketPicker.error = null;
  }

  function moveTicketPickerProjectSelection(delta) {
    const projectCount = state.ticketPicker.projects.length + 1;
    if (projectCount <= 0) {
      state.ticketPicker.selectedProjectIndex = 0;
      return;
    }
    const current = Number.isFinite(state.ticketPicker.selectedProjectIndex)
      ? state.ticketPicker.selectedProjectIndex
      : 0;
    state.ticketPicker.selectedProjectIndex = (current + delta + projectCount) % projectCount;
  }

  function selectedProjectForCreate() {
    const projectIndex = state.ticketPicker.selectedProjectIndex;
    if (!Number.isFinite(projectIndex) || projectIndex <= 0) {
      return null;
    }
    return state.ticketPicker.projects[projectIndex - 1] || null;
  }

  async function submitCreateTicketFromPicker(submitMode) {
    if (!state.ticketPicker.visible || !state.ticketPicker.newTicketMode) return;
    if (state.ticketPicker.creating) return;
    const brief = String(state.ticketPicker.newTicketBrief || "").trim();
    if (!brief) {
      state.ticketPicker.error = "enter a brief description before creating a ticket";
      return;
    }

    state.ticketPicker.creating = true;
    state.ticketPicker.error = null;
    const selectedProject = selectedProjectForCreate();
    try {
      const result = await wsRequest("ticket.create", {
        brief,
        selected_project: selectedProject,
        submit_mode: submitMode,
      });
      const createdTicket = result.ticket;
      state.warning = `created ${createdTicket?.identifier || "ticket"}`;
      state.ticketPicker.newTicketMode = false;
      state.ticketPicker.newTicketBrief = "";
      await refreshSnapshot();

      if (submitMode === "create_and_start" && createdTicket) {
        const startResult = await wsRequest("ticket.start_or_resume", {
          ticket: createdTicket,
        });
        closeTicketPicker();
        state.centerPanel = "terminal";
        if (startResult.session_id) {
          state.activeSessionId = startResult.session_id;
        }
        state.warning = `${startResult.action || "started"}: ${
          startResult.ticket_identifier || createdTicket.identifier || "ticket"
        }`;
        await refreshSnapshot();
      } else {
        const ticketResult = await wsRequest("ticket.list", {}).catch(() => ({ tickets: [] }));
        state.ticketPicker.tickets = ticketResult.tickets || [];
        state.ticketPicker.selectedIndex = 0;
      }
    } catch (error) {
      state.ticketPicker.error = toMessage(error);
    } finally {
      state.ticketPicker.creating = false;
    }
  }

  async function startSelectedTicketFromPicker() {
    if (!state.ticketPicker.visible || state.ticketPicker.tickets.length === 0) return;
    const ticket = state.ticketPicker.tickets[state.ticketPicker.selectedIndex];
    if (!ticket) return;

    try {
      const result = await wsRequest("ticket.start_or_resume", { ticket });
      state.warning = `${result.action || "started"}: ${result.ticket_identifier || ticket.identifier}`;
      closeTicketPicker();
      state.centerPanel = "terminal";
      if (result.session_id) state.activeSessionId = result.session_id;
      await refreshSnapshot();
    } catch (error) {
      state.ticketPicker.error = toMessage(error);
    }
  }

  async function archiveSelectedTicketFromPicker() {
    if (!state.ticketPicker.visible || state.ticketPicker.tickets.length === 0) return;
    const ticket = state.ticketPicker.tickets[state.ticketPicker.selectedIndex];
    if (!ticket) return;
    if (!window.confirm(`Archive ${ticket.identifier}?`)) {
      return;
    }
    try {
      await wsRequest("ticket.archive", { ticket });
      state.ticketPicker.tickets.splice(state.ticketPicker.selectedIndex, 1);
      if (state.ticketPicker.selectedIndex >= state.ticketPicker.tickets.length) {
        state.ticketPicker.selectedIndex = Math.max(0, state.ticketPicker.tickets.length - 1);
      }
      state.warning = `archived ${ticket.identifier}`;
      await refreshSnapshot();
    } catch (error) {
      state.ticketPicker.error = toMessage(error);
    }
  }

  async function toggleDiffModal() {
    if (state.diffModal.visible) {
      state.diffModal.visible = false;
      return;
    }

    const sessionId = selectedSessionId();
    if (!sessionId) {
      showToast("No selected session for diff.");
      return;
    }

    state.diffModal.visible = true;
    state.diffModal.loading = true;
    state.diffModal.error = null;
    state.diffModal.content = "";
    state.diffModal.sessionId = sessionId;

    try {
      const result = await wsRequest("session.diff.fetch", { session_id: sessionId });
      const header = `Session: ${sessionId}\nBase branch: ${result.base_branch || "unknown"}\n\n`;
      state.diffModal.content = `${header}${result.diff || "(empty diff)"}`;
    } catch (error) {
      state.diffModal.error = toMessage(error);
    } finally {
      state.diffModal.loading = false;
    }
  }

  async function advanceSelectedSessionWorkflow() {
    const sessionId = selectedSessionId();
    if (!sessionId) {
      showToast("No selected session to advance.");
      return;
    }

    const workflowState = workflowStateForSession(sessionId);
    if (reviewStageWorkflowState(workflowState)) {
      beginMergeConfirm();
      return;
    }

    if (workflowState === "Done" || workflowState === "Abandoned") {
      showToast("Workflow advance ignored: session is already complete.");
      return;
    }

    try {
      await wsRequest("session.workflow.advance", { session_id: sessionId });
      state.warning = `workflow advance requested for ${sessionId}`;
      await refreshSnapshot();
    } catch (error) {
      showToast(toMessage(error));
    }
  }

  async function enqueueMergeForSelectedSession(sessionOverride = null) {
    const sessionId = sessionOverride || selectedSessionId();
    if (!sessionId) {
      showToast("No selected review session to merge.");
      return;
    }
    try {
      await wsRequest("session.merge.enqueue", { session_id: sessionId });
      state.warning = `merge queued for ${sessionId}`;
      await refreshSnapshot();
    } catch (error) {
      showToast(toMessage(error));
    }
  }

  async function archiveSelectedSession(sessionOverride = null) {
    const sessionId = sessionOverride || selectedSessionId();
    if (!sessionId) {
      showToast("No selected session to archive.");
      return;
    }
    try {
      await wsRequest("session.archive", { session_id: sessionId });
      state.warning = `archived ${sessionId}`;
      await refreshSnapshot();
    } catch (error) {
      showToast(toMessage(error));
    }
  }

  function beginArchiveConfirm() {
    const sessionId = selectedSessionId();
    if (!sessionId) {
      showToast("No selected session to archive.");
      return;
    }
    state.archiveConfirm.visible = true;
    state.archiveConfirm.sessionId = sessionId;
    state.archiveConfirm.action = "archive_session";
  }

  function beginMergeConfirm() {
    const sessionId = selectedSessionId();
    if (!sessionId) {
      showToast("No selected review session.");
      return;
    }
    state.archiveConfirm.visible = true;
    state.archiveConfirm.sessionId = sessionId;
    state.archiveConfirm.action = "enqueue_merge";
  }

  function cancelArchiveConfirm() {
    state.archiveConfirm.visible = false;
    state.archiveConfirm.sessionId = null;
    state.archiveConfirm.action = null;
  }

  async function confirmArchiveConfirm() {
    const sessionId = state.archiveConfirm.sessionId;
    const action = state.archiveConfirm.action;
    cancelArchiveConfirm();
    if (!sessionId) return;
    if (action === "enqueue_merge") {
      await enqueueMergeForSelectedSession(sessionId);
      return;
    }
    await archiveSelectedSession(sessionId);
    render();
  }

  async function startInspectorChatStream() {
    const row = getSelectedInboxRow();
    if (!row) {
      showToast("Select an inbox item before opening chat inspector.");
      return;
    }

    state.inspectorChat.streamId = null;
    state.inspectorChat.workItemId = row.work_item_id;
    state.inspectorChat.response = "";
    state.inspectorChat.status = "starting";

    try {
      const result = await wsRequest("supervisor.query.start", {
        invocation: {
          command_id: "supervisor.query",
          args: {
            kind: "freeform",
            query: "What is the current status of this ticket?",
            context: {
              selected_work_item_id: row.work_item_id,
              selected_session_id: row.session_id,
              scope: row.session_id ? `session:${row.session_id}` : "work-item",
            },
          },
        },
        context: {
          selected_work_item_id: row.work_item_id,
          selected_session_id: row.session_id,
          scope: row.session_id ? `session:${row.session_id}` : "work-item",
        },
      });
      state.inspectorChat.streamId = result.stream_id || null;
    } catch (error) {
      state.inspectorChat.status = "failed";
      state.inspectorChat.response = `[error] ${toMessage(error)}`;
    }
  }

  function selectedSessionId() {
    const rowSession = getSelectedInboxRow()?.session_id;
    if (rowSession) return rowSession;
    if (state.activeSessionId && sessionExists(state.activeSessionId)) return state.activeSessionId;
    return state.sessions[0]?.id || null;
  }

  function workflowStateForSession(sessionId) {
    if (!sessionId) return null;
    const projection = state.snapshot?.projection || {};
    const session = projection.sessions?.[sessionId];
    if (!session) return null;
    const workItemId = session.work_item_id;
    if (!workItemId) return null;
    return projection.work_items?.[workItemId]?.workflow_state || null;
  }

  function reviewStageWorkflowState(workflowState) {
    return ["AwaitingYourReview", "ReadyForReview", "InReview", "PendingMerge"].includes(
      String(workflowState || "")
    );
  }

  function sendEnvelope(payload) {
    if (!state.ws || state.ws.readyState !== WebSocket.OPEN) return false;
    state.ws.send(JSON.stringify(payload));
    return true;
  }

  function wsRequest(type, payload) {
    const requestId = `req-${Date.now()}-${state.requestSeq++}`;
    if (!sendEnvelope({ request_id: requestId, type, payload })) {
      return Promise.reject(new Error("websocket is not connected"));
    }

    return new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        state.pendingRequests.delete(requestId);
        reject(new Error(`request timed out: ${type}`));
      }, 25000);

      state.pendingRequests.set(requestId, {
        resolve: (value) => {
          clearTimeout(timeout);
          resolve(value);
        },
        reject: (error) => {
          clearTimeout(timeout);
          reject(error);
        },
      });
    });
  }

  function rejectAllPending(message) {
    for (const [requestId, pending] of state.pendingRequests.entries()) {
      pending.reject(new Error(message));
      state.pendingRequests.delete(requestId);
    }
  }

  function render() {
    renderTopBar();
    renderInbox();
    renderSessions();
    renderCenter();
    renderComposeRow();
    renderTicketPicker();
    renderDiffModal();
    renderArchiveConfirm();
    renderNeedsInputModal();
    renderBottomBar();
  }

  function renderTopBar() {
    els.statusLine.textContent = state.statusText || "ready";
    els.connectionState.textContent = state.connected ? "online" : "offline";
    els.connectionState.classList.toggle("connected", state.connected);
    els.modeState.textContent = state.mode.toUpperCase();
  }

  function renderInbox() {
    if (state.inboxRows.length === 0) {
      els.inboxList.innerHTML = '<p class="muted">No inbox items.</p>';
      return;
    }

    const grouped = new Map(lanes.map((lane) => [lane, []]));
    state.inboxRows.forEach((row, index) => grouped.get(row.lane).push({ row, index }));

    const html = lanes
      .map((lane) => {
        const rows = grouped.get(lane);
        if (!rows || rows.length === 0) return "";

        const rowHtml = rows
          .map(({ row, index }) => {
            const selectedClass = index === state.selectedInboxIndex ? "selected" : "";
            const workflow = row.workflow_state || "unknown";
            const sessionMeta = row.session_id ? `session ${row.session_id}` : "no session";
            return `
              <div class="inbox-row ${selectedClass}" data-inbox-index="${index}">
                <div>${escapeHtml(row.title)}</div>
                <div class="inbox-meta">${escapeHtml(row.kind)} • ${escapeHtml(workflow)} • ${escapeHtml(sessionMeta)}</div>
              </div>
            `;
          })
          .join("");

        return `
          <div class="inbox-lane">
            <div class="inbox-lane-title">${laneLabels[lane]}</div>
            ${rowHtml}
          </div>
        `;
      })
      .join("");

    els.inboxList.innerHTML = html;
  }

  function renderSessions() {
    if (state.sessions.length === 0) {
      els.sessionsList.innerHTML = '<p class="muted">No sessions.</p>';
      return;
    }

    const html = state.sessions
      .map((session) => {
        const selectedClass = session.id === state.activeSessionId ? "selected" : "";
        const status = session.status || "Unknown";
        const statusClass = `status-${String(status).toLowerCase()}`;
        return `
          <div class="session-row ${selectedClass}" data-session-id="${escapeHtml(session.id)}">
            <div>${escapeHtml(session.id)}</div>
            <div class="session-meta ${statusClass}">${escapeHtml(status)}</div>
          </div>
        `;
      })
      .join("");

    els.sessionsList.innerHTML = html;
  }

  function renderCenter() {
    if (state.globalChat.visible) {
      renderGlobalChatPanel();
      return;
    }

    switch (state.centerPanel) {
      case "diff":
        renderArtifactPanel("Diff Inspector", "Diff");
        break;
      case "tests":
        renderArtifactPanel("Test Inspector", "TestRun");
        break;
      case "pr":
        renderArtifactPanel("PR Inspector", "PR");
        break;
      case "chat":
        renderInspectorChatPanel();
        break;
      case "focus":
        renderFocusPanel();
        break;
      case "terminal":
      default:
        renderTerminalPanel();
        break;
    }
  }

  function renderTerminalPanel() {
    els.centerTitle.textContent = "Terminal";
    const sessionId = selectedSessionId();
    if (!sessionId) {
      els.centerBody.textContent = "No session selected.";
      return;
    }

    const view = ensureTerminalSession(sessionId);
    const statusLine = view.turnActive ? "[working]" : "[idle]";
    const needsInput = view.needsInputComposer
      ? `\n\n[needs input]\n${formatNeedsInput(view.needsInput)}\nRespond in the Needs Input overlay.`
      : "";
    const error = view.error ? `\n\n[stream error]\n${view.error}` : "";

    els.centerTitle.textContent = `Terminal ${sessionId}`;
    els.centerBody.textContent = `${statusLine}\n\n${view.output || "No terminal output yet."}${needsInput}${error}`;
  }

  function renderArtifactPanel(title, kind) {
    const projection = state.snapshot?.projection || {};
    const artifacts = projection.artifacts || {};
    const selected = getSelectedInboxRow();

    els.centerTitle.textContent = title;
    if (!selected) {
      els.centerBody.textContent = "No inbox item selected.";
      return;
    }

    const workItem = projection.work_items?.[selected.work_item_id];
    if (!workItem || !Array.isArray(workItem.artifacts)) {
      els.centerBody.textContent = "No artifacts available.";
      return;
    }

    const filtered = workItem.artifacts
      .map((artifactId) => artifacts[artifactId])
      .filter((artifact) => artifact && artifact.kind === kind);

    if (filtered.length === 0) {
      els.centerBody.textContent = `No ${kind} artifacts available.`;
      return;
    }

    els.centerBody.innerHTML = filtered
      .map(
        (artifact) =>
          `<div><strong>${escapeHtml(artifact.label || artifact.kind)}</strong><br /><span class="muted">${escapeHtml(
            artifact.uri || ""
          )}</span></div>`
      )
      .join("<hr />");
  }

  function renderFocusPanel() {
    const selected = getSelectedInboxRow();
    els.centerTitle.textContent = "Focus";
    if (!selected) {
      els.centerBody.textContent = "No inbox item selected.";
      return;
    }

    const projection = state.snapshot?.projection || {};
    const workItem = projection.work_items?.[selected.work_item_id] || null;

    const lines = [
      `title: ${selected.title}`,
      `kind: ${selected.kind}`,
      `workflow: ${selected.workflow_state || "unknown"}`,
      `session: ${selected.session_id || "none"}`,
      `work item: ${selected.work_item_id}`,
      `ticket: ${workItem?.ticket_id || "unknown"}`,
    ];

    els.centerBody.textContent = lines.join("\n");
  }

  function renderGlobalChatPanel() {
    els.centerTitle.textContent = "Supervisor Chat (Global)";
    const query = state.globalChat.query ? `query: ${state.globalChat.query}\n\n` : "";
    const status = state.globalChat.status ? `status: ${state.globalChat.status}\n\n` : "";
    const response = state.globalChat.response || "No response yet.";
    els.centerBody.textContent = `${status}${query}${response}`;
  }

  function renderInspectorChatPanel() {
    els.centerTitle.textContent = "Chat Inspector";
    const selected = getSelectedInboxRow();
    if (!selected) {
      els.centerBody.textContent = "No inbox item selected.";
      return;
    }

    const projection = state.snapshot?.projection || {};
    const events = Array.isArray(projection.events) ? projection.events : [];
    const chatEvents = events
      .filter((event) => event.work_item_id === selected.work_item_id)
      .filter((event) =>
        ["SupervisorQueryStarted", "SupervisorQueryChunk", "SupervisorQueryFinished", "SupervisorQueryCancelled"].includes(
          event.event_type
        )
      )
      .slice(-20)
      .map((event) => `${event.sequence}: ${event.event_type}`)
      .join("\n");

    const status = state.inspectorChat.status ? `status: ${state.inspectorChat.status}\n\n` : "";
    const response = state.inspectorChat.response || "No active chat response.";

    els.centerBody.textContent = `${status}${chatEvents || "No supervisor events captured yet."}\n\n${response}`;
  }

  function renderComposeRow() {
    const showCompose = state.mode === "insert" && (state.globalChat.visible || state.centerPanel === "terminal");
    els.composeRow.classList.toggle("hidden", !showCompose);
    if (!showCompose) return;

    const composer = activeNeedsInputComposer();
    if (composer?.noteMode) {
      const draft = currentNeedsInputDraft(composer) || { note: "" };
      els.composeInput.value = draft.note;
      els.composeInput.placeholder = "Type note for selected question...";
      return;
    }

    if (state.globalChat.visible) {
      els.composeInput.value = state.globalChat.input;
      els.composeInput.placeholder = "Type supervisor query...";
    } else {
      els.composeInput.value = terminalCompose.input;
      els.composeInput.placeholder = "Type terminal input...";
    }
  }

  function renderTicketPicker() {
    els.ticketPicker.classList.toggle("hidden", !state.ticketPicker.visible);
    if (!state.ticketPicker.visible) return;

    if (state.ticketPicker.newTicketMode) {
      const projectOptions = ["No Project", ...state.ticketPicker.projects];
      const maxProjectIndex = Math.max(0, projectOptions.length - 1);
      if (state.ticketPicker.selectedProjectIndex > maxProjectIndex) {
        state.ticketPicker.selectedProjectIndex = maxProjectIndex;
      }
      const selectedProject = projectOptions[state.ticketPicker.selectedProjectIndex] || "No Project";
      const creating = state.ticketPicker.creating ? "<p>Creating ticket...</p>" : "";
      const error = state.ticketPicker.error ? `<p class="warning">${escapeHtml(state.ticketPicker.error)}</p>` : "";
      const projectsHtml = projectOptions
        .map((project, index) => {
          const selectedClass = index === state.ticketPicker.selectedProjectIndex ? "selected" : "";
          return `<div class="ticket-row ${selectedClass}" data-project-index="${index}">
            <div>${escapeHtml(project)}</div>
          </div>`;
        })
        .join("");

      els.ticketPickerBody.innerHTML = `
        <div class="inbox-lane-title">Create Ticket</div>
        <p class="ticket-meta">Enter: create only • Shift+Enter: create + start • Esc: cancel</p>
        ${creating}
        ${error}
        <div class="inbox-lane-title">Project (${escapeHtml(selectedProject)})</div>
        ${projectsHtml}
        <div class="inbox-lane-title">Brief</div>
        <pre class="monospace">${escapeHtml(state.ticketPicker.newTicketBrief || "")}</pre>
      `;
      return;
    }

    if (state.ticketPicker.loading) {
      els.ticketPickerBody.innerHTML = '<p class="muted">Loading tickets...</p>';
      return;
    }

    if (state.ticketPicker.error) {
      els.ticketPickerBody.innerHTML = `<p>${escapeHtml(state.ticketPicker.error)}</p>`;
      return;
    }

    if (state.ticketPicker.tickets.length === 0) {
      els.ticketPickerBody.innerHTML = '<p class="muted">No unfinished tickets.</p>';
      return;
    }

    els.ticketPickerBody.innerHTML = state.ticketPicker.tickets
      .map((ticket, index) => {
        const selectedClass = index === state.ticketPicker.selectedIndex ? "selected" : "";
        return `
          <div class="ticket-row ${selectedClass}" data-ticket-index="${index}">
            <div>${escapeHtml(ticket.identifier)} — ${escapeHtml(ticket.title)}</div>
            <div class="ticket-meta">${escapeHtml(ticket.state)} • ${escapeHtml(ticket.project || "No Project")}</div>
          </div>
        `;
      })
      .join("");
  }

  function renderDiffModal() {
    els.diffModal.classList.toggle("hidden", !state.diffModal.visible);
    if (!state.diffModal.visible) return;

    if (state.diffModal.loading) {
      els.diffContent.textContent = "Loading diff...";
      return;
    }

    if (state.diffModal.error) {
      els.diffContent.textContent = state.diffModal.error;
      return;
    }

    els.diffContent.textContent = state.diffModal.content || "No diff available.";
  }

  function renderArchiveConfirm() {
    els.archiveConfirm.classList.toggle("hidden", !state.archiveConfirm.visible);
    if (!state.archiveConfirm.visible) return;
    const action = state.archiveConfirm.action || "archive_session";
    const sessionId = state.archiveConfirm.sessionId;
    if (action === "enqueue_merge") {
      els.archiveConfirmTitle.textContent = "Queue Merge";
      els.archiveConfirmMessage.textContent = sessionId
        ? `Queue merge for review session ${sessionId}?`
        : "Queue merge for selected review session?";
      els.archiveConfirmYes.textContent = "queue merge (y)";
      els.archiveConfirmNo.textContent = "cancel (n)";
      return;
    }

    els.archiveConfirmTitle.textContent = "Archive Session";
    els.archiveConfirmMessage.textContent = sessionId
      ? `Archive session ${sessionId}?`
      : "Archive selected session?";
    els.archiveConfirmYes.textContent = "archive (y)";
    els.archiveConfirmNo.textContent = "cancel (n)";
  }

  function renderNeedsInputModal() {
    const composer = state.centerPanel === "terminal" ? activeNeedsInputComposer() : null;
    const visible = Boolean(composer);
    els.needsInputModal.classList.toggle("hidden", !visible);
    if (!visible || !composer) return;

    const question = composer.questions[composer.currentQuestionIndex];
    if (!question) {
      els.needsInputBody.innerHTML = "<p class='muted'>No pending questions.</p>";
      els.needsInputFooter.textContent = "Esc: normal";
      return;
    }

    const draft = currentNeedsInputDraft(composer) || { selectedOptionIndex: null, optionCursor: 0, note: "" };
    const options = Array.isArray(question.options) ? question.options : [];
    const optionsHtml =
      options.length === 0
        ? "<p class='muted'>No options. Provide a note response.</p>"
        : `<div class="needs-input-options">${options
            .map((option, index) => {
              const cursorClass = index === draft.optionCursor ? "cursor" : "";
              const selectedClass = index === draft.selectedOptionIndex ? "selected" : "";
              const description = option.description ? `<div class="inbox-meta">${escapeHtml(option.description)}</div>` : "";
              return `<div class="needs-input-option ${cursorClass} ${selectedClass}">
                <div>${escapeHtml(option.label)}</div>
                ${description}
              </div>`;
            })
            .join("")}</div>`;

    const sessionId = selectedSessionId() || "unknown";
    const totalQuestions = composer.questions.length;
    els.needsInputTitle.textContent = `Needs Input • ${sessionId}`;
    els.needsInputBody.innerHTML = `
      <div class="needs-input-question">
        <div class="needs-input-header">Question ${composer.currentQuestionIndex + 1}/${totalQuestions}: ${escapeHtml(
      question.header || "Input"
    )}</div>
        <div>${escapeHtml(question.question || "(no prompt text provided)")}</div>
        ${optionsHtml}
        <div class="needs-input-note">Note: ${
          draft.note.trim().length > 0 ? escapeHtml(draft.note) : "<span class='muted'>(empty)</span>"
        }</div>
        ${composer.error ? `<div class="warning">${escapeHtml(composer.error)}</div>` : ""}
      </div>
    `;

    els.needsInputFooter.textContent =
      "h/l questions • j/k options • Space select • i edit note • Enter next/submit • Esc normal";
  }

  function renderBottomBar() {
    els.warningLine.textContent = state.warning || "ready";

    const hints = [];
    if (state.keySequence.length > 0) {
      hints.push(`sequence: ${state.keySequence.join(" ")}`);
    }
    if (state.keyHint) {
      hints.push(`prefix: ${state.keyHint}`);
    }

    if (state.ticketPicker.visible && state.ticketPicker.newTicketMode) {
      hints.push(
        "ticket create: type brief",
        "j/k or Tab project",
        "Enter create",
        "Shift+Enter create+start",
        "Esc cancel"
      );
    } else if (state.ticketPicker.visible) {
      hints.push("ticket picker: j/k move", "enter start", "x archive", "Esc close");
    } else if (state.archiveConfirm.visible) {
      const merge = state.archiveConfirm.action === "enqueue_merge";
      hints.push(
        merge ? "merge confirm: y/Enter queue" : "archive confirm: y/Enter confirm",
        "n/Esc cancel"
      );
    } else if (activeNeedsInputComposer()) {
      const composer = activeNeedsInputComposer();
      if (composer?.noteMode) {
        hints.push("needs input note: type", "Enter done", "Esc cancel");
      } else {
        hints.push("needs input: h/l question", "j/k option", "Space select", "i note", "Enter submit");
      }
    } else {
      hints.push("j/k move", "i insert", "Esc normal", "s ticket picker", "c chat", "v d/t/p/c views");
    }

    els.hintLine.textContent = hints.join(" • ");
  }

  function keyToken(event) {
    if (event.ctrlKey || event.metaKey || event.altKey) {
      return null;
    }

    if (event.key === "ArrowDown") return "down";
    if (event.key === "ArrowUp") return "up";
    if (event.key === "ArrowLeft") return "left";
    if (event.key === "ArrowRight") return "right";
    if (event.key === "Enter") return "enter";
    if (event.key === "Tab" && event.shiftKey) return "backtab";
    if (event.key === "Tab") return "tab";
    if (event.key === "Backspace") return "backspace";
    if (event.key === "Home") return "home";
    if (event.key === "End") return "end";
    if (event.key === "PageUp") return "pageup";
    if (event.key === "PageDown") return "pagedown";
    if (event.key === " ") return " ";

    if (event.key.length === 1) {
      return event.key;
    }

    return null;
  }

  function formatNeedsInput(needsInput) {
    if (!needsInput) return "";
    const lines = [];
    if (needsInput.question) {
      lines.push(`question: ${needsInput.question}`);
    }
    if (Array.isArray(needsInput.questions) && needsInput.questions.length > 0) {
      for (const question of needsInput.questions) {
        lines.push(`- ${question.header}: ${question.question}`);
      }
    } else if (Array.isArray(needsInput.options) && needsInput.options.length > 0) {
      lines.push(`options: ${needsInput.options.join(" | ")}`);
    }
    return lines.join("\n");
  }

  function formatNotification(payload) {
    const prefix = payload.level ? String(payload.level).toLowerCase() : "info";
    return `${prefix}: ${payload.message || ""}`.trim();
  }

  function escapeHtml(value) {
    return String(value || "")
      .replaceAll("&", "&amp;")
      .replaceAll("<", "&lt;")
      .replaceAll(">", "&gt;")
      .replaceAll('"', "&quot;")
      .replaceAll("'", "&#39;");
  }

  function showToast(message) {
    if (!message) return;
    els.toast.textContent = message;
    els.toast.classList.remove("hidden");
    clearTimeout(showToast.timeoutId);
    showToast.timeoutId = setTimeout(() => {
      els.toast.classList.add("hidden");
    }, 4200);
  }

  function toMessage(error) {
    if (error instanceof Error) return error.message;
    if (typeof error === "string") return error;
    try {
      return JSON.stringify(error);
    } catch {
      return "unknown error";
    }
  }

  init();
})();
