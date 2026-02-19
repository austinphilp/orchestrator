.PHONY: harness-logs-run harness-logs-tail reset-dev-state

HARNESS_RAW_LOG_FILE ?= .orchestrator/logs/harness-raw.log
HARNESS_NORMALIZED_LOG_FILE ?= .orchestrator/logs/harness-normalized.log
ORCHESTRATOR_DATA_DIR ?= $(HOME)/.local/share/orchestrator
ORCHESTRATOR_DB_PATH ?= $(ORCHESTRATOR_DATA_DIR)/orchestrator-events.db
ORCHESTRATOR_WORKSPACE_DIR ?= $(ORCHESTRATOR_DATA_DIR)/workspace
ORCHESTRATOR_WORKTREES_DIR ?= $(ORCHESTRATOR_WORKSPACE_DIR)/.orchestrator/worktrees

harness-logs-run:
	mkdir -p "$(dir $(HARNESS_RAW_LOG_FILE))"
	touch "$(HARNESS_RAW_LOG_FILE)" "$(HARNESS_NORMALIZED_LOG_FILE)"
	ORCHESTRATOR_HARNESS_LOG_NORMALIZED_EVENTS=true \
	ORCHESTRATOR_HARNESS_LOG_RAW_EVENTS=true \
	cargo run -p orchestrator-app

harness-logs-tail:
	mkdir -p "$(dir $(HARNESS_RAW_LOG_FILE))"
	touch "$(HARNESS_RAW_LOG_FILE)" "$(HARNESS_NORMALIZED_LOG_FILE)"
	tail -f "$(HARNESS_RAW_LOG_FILE)" "$(HARNESS_NORMALIZED_LOG_FILE)"

reset-dev-state:
	rm -f "$(ORCHESTRATOR_DB_PATH)"
	mkdir -p "$(dir $(ORCHESTRATOR_DB_PATH))"
	touch "$(ORCHESTRATOR_DB_PATH)"
	rm -rf "$(ORCHESTRATOR_WORKTREES_DIR)"
	mkdir -p "$(ORCHESTRATOR_WORKTREES_DIR)"
	@printf "Reset complete:\n"
	@printf "  DB: %s\n" "$(ORCHESTRATOR_DB_PATH)"
	@printf "  Worktrees: %s\n" "$(ORCHESTRATOR_WORKTREES_DIR)"
