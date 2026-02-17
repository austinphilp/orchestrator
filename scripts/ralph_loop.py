#!/usr/bin/env python3
"""Ralph loop automation for Orchestrator Linear tickets.

Workflow per ticket (sequential, alphanumeric):
1) Generate implementation plan with Codex (read-only pass)
2) Update Linear ticket with plan block
3) Move ticket to In Progress
4) Ask Codex to implement the ticket in this repo
5) Ask Codex to review and harden the implementation
6) Commit and push exactly one commit for the ticket
7) Move ticket to Done

This script is intentionally opinionated and stops on the first failure by default.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import re
import shlex
import subprocess
import tempfile
import sys
import textwrap
import time
import urllib.error
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Any

LINEAR_ENDPOINT = "https://api.linear.app/graphql"
PLAN_START = "<!-- ralph-plan:start -->"
PLAN_END = "<!-- ralph-plan:end -->"
SUPERVISOR_TICKET_IDS = "AP-180,AP-181,AP-182,AP-183,AP-184,AP-185,AP-186"


class RalphError(RuntimeError):
    pass


@dataclass
class Issue:
    id: str
    identifier: str
    title: str
    description: str
    state_name: str
    state_type: str
    project_id: str
    project_name: str

    @property
    def short_title(self) -> str:
        if ":" in self.title:
            return self.title.split(":", 1)[1].strip()
        return self.title


@dataclass
class WorktreeSnapshot:
    baseline_commit: str | None
    untracked: set[str]


class LinearClient:
    def __init__(self, token: str, endpoint: str = LINEAR_ENDPOINT, timeout: float = 60.0):
        self.token = token
        self.endpoint = endpoint
        self.timeout = timeout

    def gql(self, query: str, variables: dict[str, Any] | None = None, retries: int = 5) -> dict[str, Any]:
        payload: dict[str, Any] = {"query": query}
        if variables is not None:
            payload["variables"] = variables

        for attempt in range(retries):
            request = urllib.request.Request(
                self.endpoint,
                data=json.dumps(payload).encode("utf-8"),
                headers={
                    "Authorization": self.token,
                    "Content-Type": "application/json",
                },
                method="POST",
            )
            try:
                with urllib.request.urlopen(request, timeout=self.timeout) as response:
                    raw = response.read().decode("utf-8")
            except urllib.error.HTTPError as e:
                raw = e.read().decode("utf-8", errors="replace")
                try:
                    data = json.loads(raw)
                except json.JSONDecodeError:
                    if attempt + 1 == retries:
                        raise RalphError(f"Linear HTTP error (non-JSON): {raw}")
                    time.sleep(1.5 * (attempt + 1))
                    continue
                if self._is_rate_limited(data) and attempt + 1 < retries:
                    time.sleep(1.5 * (attempt + 1))
                    continue
                raise RalphError(f"Linear HTTP error: {json.dumps(data, indent=2)}")

            data = json.loads(raw)
            if data.get("errors"):
                if self._is_rate_limited(data) and attempt + 1 < retries:
                    time.sleep(1.5 * (attempt + 1))
                    continue
                raise RalphError(f"Linear GraphQL error: {json.dumps(data, indent=2)}")
            return data["data"]

        raise RalphError("Linear request failed after retries")

    @staticmethod
    def _is_rate_limited(response: dict[str, Any]) -> bool:
        for err in response.get("errors", []):
            code = (err.get("extensions") or {}).get("code")
            if code == "RATELIMITED":
                return True
        return False

    def get_workflow_states(self, team_id: str) -> list[dict[str, str]]:
        query = """
        query($teamId:ID!){
          workflowStates(first:200, filter:{ team:{ id:{ eq:$teamId } } }) {
            nodes { id name type }
          }
        }
        """
        data = self.gql(query, {"teamId": team_id})
        return data["workflowStates"]["nodes"]

    def list_pending_issues(
        self,
        team_id: str,
        prefix: str,
        identifier_prefix: str | None = None,
        project_id: str | None = None,
        identifier_filter: set[str] | None = None,
    ) -> list[Issue]:
        query = """
        query($teamId:ID!){
          issues(
            first:250,
            filter:{
              archivedAt:{ null:true },
              team:{ id:{ eq:$teamId } },
              state:{ type:{ nin:["completed","canceled"] } }
            }
          ){
            nodes {
              id
              identifier
              title
              description
              state { name type }
              project { id name }
            }
          }
        }
        """
        data = self.gql(query, {"teamId": team_id})
        out: list[Issue] = []
        identifier_filter = identifier_filter or set()
        for n in data["issues"]["nodes"]:
            title = n["title"]
            identifier = n["identifier"]
            project = n.get("project")
            issue_project_id = project.get("id") if project else None

            if project_id and issue_project_id != project_id:
                continue
            if identifier_filter and identifier not in identifier_filter:
                continue
            if prefix and prefix not in title:
                if not identifier_prefix or not identifier.startswith(identifier_prefix):
                    continue
            if identifier_prefix and not identifier.startswith(identifier_prefix):
                if not prefix or prefix not in title:
                    continue
            out.append(
                Issue(
                    id=n["id"],
                    identifier=identifier,
                    title=title,
                    description=n.get("description") or "",
                    state_name=n["state"]["name"],
                    state_type=n["state"]["type"],
                    project_id=issue_project_id or "",
                    project_name=(project.get("name") or "") if project else "",
                )
            )
        return out

    def list_project_plan_outline(
        self,
        team_id: str,
        prefix: str,
        identifier_prefix: str | None = None,
        project_id: str | None = None,
        identifier_filter: set[str] | None = None,
    ) -> list[dict[str, str]]:
        query = """
        query($teamId:ID!){
          issues(
            first:250,
            filter:{
              archivedAt:{ null:true },
              team:{ id:{ eq:$teamId } },
            }
          ){
            nodes {
              identifier
              title
              state { name type }
              project { id name }
            }
          }
        }
        """
        data = self.gql(query, {"teamId": team_id})
        out: list[dict[str, str]] = []
        identifier_filter = identifier_filter or set()
        for n in data["issues"]["nodes"]:
            title = n["title"]
            identifier = n["identifier"]
            project = n.get("project")
            issue_project_id = project.get("id") if project else None

            if project_id and issue_project_id != project_id:
                continue
            if identifier_filter and identifier not in identifier_filter:
                continue
            if prefix and prefix not in title:
                if not identifier_prefix or not identifier.startswith(identifier_prefix):
                    continue
            if identifier_prefix and not identifier.startswith(identifier_prefix):
                if not prefix or prefix not in title:
                    continue
            out.append(
                {
                    "identifier": n["identifier"],
                    "title": n["title"],
                    "state_name": n["state"]["name"],
                    "state_type": n["state"]["type"],
                }
            )
        return out

    def update_issue_description(self, issue_id: str, description: str) -> None:
        mutation = """
        mutation($id:String!, $description:String!){
          issueUpdate(id:$id, input:{ description:$description }) {
            success
            issue { id identifier }
          }
        }
        """
        data = self.gql(mutation, {"id": issue_id, "description": description})
        if not data["issueUpdate"]["success"]:
            raise RalphError(f"issueUpdate description failed for {issue_id}")

    def move_issue_state(self, issue_id: str, state_id: str) -> None:
        mutation = """
        mutation($id:String!, $stateId:String!){
          issueUpdate(id:$id, input:{ stateId:$stateId }) {
            success
            issue { id identifier state { id name type } }
          }
        }
        """
        data = self.gql(mutation, {"id": issue_id, "stateId": state_id})
        if not data["issueUpdate"]["success"]:
            raise RalphError(f"issueUpdate state failed for {issue_id}")

    def add_comment(self, issue_id: str, body: str) -> None:
        mutation = """
        mutation($issueId:String!, $body:String!){
          commentCreate(input:{ issueId:$issueId, body:$body }){
            success
            comment { id url }
          }
        }
        """
        data = self.gql(mutation, {"issueId": issue_id, "body": body})
        if not data["commentCreate"]["success"]:
            raise RalphError(f"commentCreate failed for {issue_id}")


def run_cmd(cmd: list[str], cwd: Path, check: bool = True, env: dict[str, str] | None = None) -> subprocess.CompletedProcess[str]:
    process = subprocess.run(
        cmd,
        cwd=str(cwd),
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if check and process.returncode != 0:
        raise RalphError(
            f"Command failed ({process.returncode}): {' '.join(shlex.quote(c) for c in cmd)}\n"
            f"stdout:\n{process.stdout}\n"
            f"stderr:\n{process.stderr}"
        )
    return process


def ensure_clean_worktree(repo: Path) -> None:
    result = run_cmd(["git", "status", "--porcelain"], cwd=repo)
    if result.stdout.strip():
        raise RalphError("Git worktree is dirty. Commit/stash changes before running Ralph loop.")


def current_branch(repo: Path) -> str:
    result = run_cmd(["git", "rev-parse", "--abbrev-ref", "HEAD"], cwd=repo)
    branch = result.stdout.strip()
    if not branch or branch == "HEAD":
        raise RalphError("Detached HEAD is not supported for Ralph loop.")
    return branch


def sort_key_from_title(title: str, prefix: str) -> tuple[Any, ...]:
    # Expected child ticket format: ORCH2-A1: ...
    # Epic format: ORCH2-EPIC A: ...
    child_pat = re.compile(rf"^{re.escape(prefix)}([A-Z]+)(\d+)\b")
    epic_pat = re.compile(rf"^{re.escape(prefix)}EPIC\s+([A-Z]+)\b")
    if m := child_pat.match(title):
        letters = m.group(1)
        number = int(m.group(2))
        return (0, letters, number, title)
    if m := epic_pat.match(title):
        letters = m.group(1)
        return (1, letters, 0, title)
        return (2, title)


def sort_key_for_issue(issue: Issue, prefix: str, identifier_prefix: str | None = None) -> tuple[Any, ...]:
    m = re.search(r"-(\d+)\b", issue.identifier)
    if m:
        return (0, int(m.group(1)), issue.identifier)
    return sort_key_from_title(issue.title, prefix)


def is_epic_title(title: str, prefix: str) -> bool:
    if re.search(r"\bEPIC\b", title, flags=re.IGNORECASE):
        return True
    if prefix and re.match(rf"^{re.escape(prefix)}EPIC\s+[A-Z]+\b", title):
        return True
    return False


def parse_identifier_filter(value: str | None) -> set[str]:
    if not value:
        return set()
    return {entry.strip().upper() for entry in value.split(",") if entry.strip()}


def compose_plan_prompt(issue: Issue, repo_root: Path, design_plan_path: Path) -> str:
    return textwrap.dedent(
        f"""
        You are preparing an implementation plan for one Linear ticket.

        Ticket:
        - Identifier: {issue.identifier}
        - Title: {issue.title}
        - Current description:
        {issue.description or '(none)'}

        Repository root: {repo_root}
        Design plan file: {design_plan_path}

        Task:
        1) Read relevant code and design plan sections.
        2) Produce a concrete implementation plan for THIS ticket only.
        3) Include:
           - scope and non-scope
           - exact files/modules expected to change
           - step-by-step implementation sequence
           - test plan and acceptance checks
           - dependency/ordering notes if this ticket is blocked by others
        4) Do NOT modify files.

        Output markdown only, concise but complete.
        """
    ).strip()


def compose_impl_prompt(issue: Issue, repo_root: Path, design_plan_path: Path) -> str:
    return textwrap.dedent(
        f"""
        Implement Linear ticket {issue.identifier}: {issue.title}

        Constraints:
        - Work only in this repository: {repo_root}
        - Follow the design plan: {design_plan_path}
        - Implement only this ticket's scope.
        - Run relevant tests/checks.
        - Do NOT run git commit.
        - Do NOT run git push.

        Ticket description (source of truth):
        {issue.description or '(none)'}

        At the end, print a short summary of changed files and tests run.
        """
    ).strip()


def format_future_plan_outline(outline: list[dict[str, str]], prefix: str) -> str:
    rows = sorted(outline, key=lambda x: sort_key_from_title(x["title"], prefix))
    lines = [
        f"- {row['identifier']} | {row['title']} | state={row['state_name']} ({row['state_type']})"
        for row in rows
    ]
    return "\n".join(lines)


def compose_review_prompt(
    issue: Issue,
    repo_root: Path,
    design_plan_path: Path,
    future_plan_outline: str,
) -> str:
    return textwrap.dedent(
        f"""
        Review and harden the current implementation for Linear ticket {issue.identifier}: {issue.title}

        Scope and intent:
        - Primary focus: this ticket's current uncommitted implementation.
        - You MAY change code outside the immediate ticket area when it materially improves maintainability,
          extensibility, architecture coherence, or regression risk.

        You must perform a senior-level review with these lenses:
        A) The codebase as a whole (architecture fit, consistency, extensibility)
        B) The specific changes introduced for this ticket (correctness, style, code smell)
        C) The future plan described by Linear tickets (avoid painting us into a corner)

        Repository root: {repo_root}
        Design plan file: {design_plan_path}

        Future plan context (current ORCH2 project tickets):
        {future_plan_outline}

        Required actions:
        1) Inspect current repo state and uncommitted changes.
        2) Apply any code improvements you judge necessary.
        3) Look for likely regressions and test coverage gaps.
        4) Add/adjust tests when warranted.
        5) Run relevant checks/tests.
        6) Do NOT run git commit.
        7) Do NOT run git push.

        Final output format:
        - Findings addressed (ordered by severity)
        - Residual risks (if any)
        - Files changed
        - Tests/checks run
        """
    ).strip()


def review_ticket_with_codex(
    codex_bin: str,
    repo: Path,
    issue: Issue,
    design_plan_path: Path,
    model: str | None,
    future_plan_outline: str,
) -> None:
    cmd = [codex_bin]
    if model:
        cmd.extend(["-m", model])
    cmd.extend(
        [
            "exec",
            "-C",
            str(repo),
            "--dangerously-bypass-approvals-and-sandbox",
            compose_review_prompt(issue, repo, design_plan_path, future_plan_outline),
        ]
    )
    run_cmd(cmd, cwd=repo)


def generate_plan_with_codex(codex_bin: str, repo: Path, issue: Issue, design_plan_path: Path, model: str | None) -> str:
    with tempfile.NamedTemporaryFile(prefix="ralph-plan-", suffix=".md", delete=False) as tmp:
        plan_path = Path(tmp.name)
    try:
        cmd = [codex_bin]
        if model:
            cmd.extend(["-m", model])
        cmd.extend(
            [
                "exec",
                "-C",
                str(repo),
                "-s",
                "read-only",
                "--output-last-message",
                str(plan_path),
                compose_plan_prompt(issue, repo, design_plan_path),
            ]
        )
        run_cmd(cmd, cwd=repo)
        text = plan_path.read_text(encoding="utf-8").strip()
        if not text:
            raise RalphError(f"Codex plan output was empty for {issue.identifier}")
        return text
    finally:
        if plan_path.exists():
            plan_path.unlink()


def apply_plan_block(existing_description: str, plan_markdown: str, issue_identifier: str) -> str:
    now = dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%d %H:%M UTC")
    block = textwrap.dedent(
        f"""
        {PLAN_START}
        ## Ralph Implementation Plan ({now})

        Generated for `{issue_identifier}`.

        {plan_markdown.strip()}
        {PLAN_END}
        """
    ).strip()

    if PLAN_START in existing_description and PLAN_END in existing_description:
        pattern = re.compile(rf"{re.escape(PLAN_START)}.*?{re.escape(PLAN_END)}", re.DOTALL)
        return pattern.sub(block, existing_description)

    if existing_description.strip():
        return existing_description.rstrip() + "\n\n" + block + "\n"
    return block + "\n"


def implement_ticket_with_codex(codex_bin: str, repo: Path, issue: Issue, design_plan_path: Path, model: str | None) -> None:
    cmd = [codex_bin]
    if model:
        cmd.extend(["-m", model])
    cmd.extend(
        [
            "exec",
            "-C",
            str(repo),
            "--dangerously-bypass-approvals-and-sandbox",
            compose_impl_prompt(issue, repo, design_plan_path),
        ]
    )
    run_cmd(cmd, cwd=repo)


def create_single_commit(repo: Path, issue: Issue) -> str:
    status = run_cmd(["git", "status", "--porcelain"], cwd=repo)
    if not status.stdout.strip():
        raise RalphError(
            f"No file changes detected after implementation for {issue.identifier}; refusing to create empty commit."
        )

    run_cmd(["git", "add", "-A"], cwd=repo)
    message = f"{issue.identifier}: {issue.short_title}"
    run_cmd(["git", "commit", "-m", message], cwd=repo)
    sha = run_cmd(["git", "rev-parse", "HEAD"], cwd=repo).stdout.strip()
    return sha


def push_current_branch(repo: Path, branch: str) -> None:
    push = run_cmd(["git", "push"], cwd=repo, check=False)
    if push.returncode == 0:
        return

    # Fallback if upstream is missing.
    fallback = run_cmd(["git", "push", "-u", "origin", branch], cwd=repo, check=False)
    if fallback.returncode != 0:
        raise RalphError(
            "git push failed.\n"
            f"First attempt stderr:\n{push.stderr}\n"
            f"Fallback stderr:\n{fallback.stderr}"
        )


def snapshot_worktree(repo: Path) -> WorktreeSnapshot:
    baseline = run_cmd(["git", "stash", "create"], cwd=repo).stdout.strip() or None
    untracked_raw = run_cmd(["git", "ls-files", "--others", "--exclude-standard"], cwd=repo).stdout
    untracked = {line.strip() for line in untracked_raw.splitlines() if line.strip()}
    return WorktreeSnapshot(baseline_commit=baseline, untracked=untracked)


def summarize_review_stage_changes(repo: Path, before: WorktreeSnapshot, issue: Issue) -> str:
    changed_entries: list[str] = []
    shortstat = ""

    if before.baseline_commit:
        shortstat = run_cmd(["git", "diff", "--shortstat", before.baseline_commit], cwd=repo).stdout.strip()
        name_status = run_cmd(["git", "diff", "--name-status", before.baseline_commit], cwd=repo).stdout
        changed_entries.extend([line.strip() for line in name_status.splitlines() if line.strip()])

    post_untracked_raw = run_cmd(["git", "ls-files", "--others", "--exclude-standard"], cwd=repo).stdout
    post_untracked = {line.strip() for line in post_untracked_raw.splitlines() if line.strip()}
    for path in sorted(post_untracked - before.untracked):
        changed_entries.append(f"A\t{path} (new untracked)")

    if not changed_entries:
        return (
            f"Ralph review stage completed for `{issue.identifier}` and made no additional file changes "
            "beyond the implementation pass."
        )

    if not shortstat:
        shortstat = f"{len(changed_entries)} file change entries"

    max_entries = 30
    display_entries = changed_entries[:max_entries]
    truncated = len(changed_entries) - len(display_entries)

    lines = [
        f"Ralph review stage summary for `{issue.identifier}`:",
        "",
        f"- Diff summary: {shortstat}",
        "- File-level changes introduced during review stage:",
    ]
    lines.extend([f"  - `{entry}`" for entry in display_entries])
    if truncated > 0:
        lines.append(f"  - `... +{truncated} more entries`")

    return "\n".join(lines)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Run Ralph loop over pending ORCH2 Linear tickets.")
    parser.add_argument("--repo", default=".", help="Path to repository root (default: current directory)")
    parser.add_argument("--design-plan", default="orchestrator-design-plan.md", help="Design plan markdown file")
    parser.add_argument("--project-id", default="", help="Optional Linear project ID filter")
    parser.add_argument(
        "--identifier-ids",
        default=SUPERVISOR_TICKET_IDS,
        help="Comma-separated ticket identifiers to include (defaults to AP-180..AP-186).",
    )
    parser.add_argument("--team-id", default="85372e13-5228-4d18-b518-0e845c2c0683", help="Linear team ID")
    parser.add_argument("--prefix", default="", help="Ticket title prefix to process")
    parser.add_argument("--identifier-prefix", default="AP-", help="Ticket identifier prefix to include (e.g. AP-)")
    parser.add_argument("--in-progress-name", default="In Progress", help="Linear state name used for in-progress")
    parser.add_argument("--done-name", default="Done", help="Linear state name used for completion")
    parser.add_argument("--include-epics", action="store_true", help="Include EPIC tickets in loop")
    parser.add_argument("--codex-bin", default="codex", help="Codex CLI binary")
    parser.add_argument("--codex-model", default=None, help="Optional Codex model override")
    parser.add_argument("--max-tickets", type=int, default=7, help="Max number of tickets to process (0 = all)")
    parser.add_argument("--continue-on-error", action="store_true", help="Continue with next ticket on failure")
    parser.add_argument("--dry-run", action="store_true", help="Plan/list only; do not mutate Linear/git or run Codex")
    parser.add_argument(
        "--allow-dirty-start",
        action="store_true",
        help="Allow starting with a dirty git worktree (not recommended for real runs)",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()

    token = os.getenv("LINEAR_API_KEY")
    if not token:
        raise RalphError("LINEAR_API_KEY is not set")

    repo = Path(args.repo).resolve()
    design_plan = (repo / args.design_plan).resolve()
    if not design_plan.exists():
        raise RalphError(f"Design plan file not found: {design_plan}")

    if args.dry_run:
        print("[dry-run] no ticket mutations, codex execution, commits, or pushes will occur")

    if args.dry_run:
        pass
    elif args.allow_dirty_start:
        print("WARNING: starting with a dirty worktree due to --allow-dirty-start")
    else:
        ensure_clean_worktree(repo)
    branch = current_branch(repo)

    linear = LinearClient(token)
    states = linear.get_workflow_states(args.team_id)
    state_by_name = {s["name"]: s["id"] for s in states}

    if args.in_progress_name not in state_by_name:
        raise RalphError(f"State '{args.in_progress_name}' not found for team {args.team_id}")
    if args.done_name not in state_by_name:
        raise RalphError(f"State '{args.done_name}' not found for team {args.team_id}")

    in_progress_id = state_by_name[args.in_progress_name]
    done_id = state_by_name[args.done_name]

    project_id = args.project_id.strip() or None
    identifier_filter = parse_identifier_filter(args.identifier_ids)
    issues = linear.list_pending_issues(
        args.team_id,
        args.prefix,
        args.identifier_prefix,
        project_id=project_id,
        identifier_filter=identifier_filter,
    )
    issues.sort(key=lambda i: sort_key_for_issue(i, args.prefix, args.identifier_prefix))

    if not args.include_epics:
        issues = [i for i in issues if not is_epic_title(i.title, args.prefix)]

    if args.max_tickets > 0:
        issues = issues[: args.max_tickets]

    if not issues:
        print("No pending tickets found matching criteria.")
        return 0

    print(f"Found {len(issues)} pending ticket(s) on branch '{branch}':")
    for i, issue in enumerate(issues, start=1):
        print(f"  {i:>2}. {issue.identifier} | {issue.title} | state={issue.state_name}")

    failures: list[str] = []

    for idx, issue in enumerate(issues, start=1):
        print(f"\n=== [{idx}/{len(issues)}] Processing {issue.identifier}: {issue.title} ===")
        try:
            if args.dry_run:
                print("[dry-run] would generate plan, update ticket, move to In Progress, implement, review/harden, commit, push, move to Done")
                continue

            # 1) Generate plan.
            print("[1/7] Generating implementation plan via Codex (read-only)...")
            plan_md = generate_plan_with_codex(args.codex_bin, repo, issue, design_plan, args.codex_model)

            # 2) Update ticket description.
            print("[2/7] Updating Linear ticket description with Ralph plan block...")
            new_description = apply_plan_block(issue.description, plan_md, issue.identifier)
            linear.update_issue_description(issue.id, new_description)

            # 3) Move to In Progress.
            print(f"[3/7] Moving ticket to '{args.in_progress_name}'...")
            linear.move_issue_state(issue.id, in_progress_id)

            # 4) Implement.
            print("[4/7] Implementing ticket via Codex...")
            implement_ticket_with_codex(args.codex_bin, repo, issue, design_plan, args.codex_model)

            # 5) Review + harden.
            print("[5/7] Running Codex review/hardening pass...")
            review_baseline = snapshot_worktree(repo)
            plan_outline = linear.list_project_plan_outline(
                args.team_id,
                args.prefix,
                args.identifier_prefix,
                project_id=project_id,
                identifier_filter=identifier_filter,
            )
            review_ticket_with_codex(
                args.codex_bin,
                repo,
                issue,
                design_plan,
                args.codex_model,
                format_future_plan_outline(plan_outline, args.prefix),
            )
            review_summary = summarize_review_stage_changes(repo, review_baseline, issue)
            linear.add_comment(issue.id, review_summary)

            # 6) Commit + push.
            print("[6/7] Creating one commit and pushing...")
            sha = create_single_commit(repo, issue)
            push_current_branch(repo, branch)

            # 7) Move to Done and annotate.
            print(f"[7/7] Moving ticket to '{args.done_name}' and adding commit note...")
            linear.add_comment(
                issue.id,
                f"Ralph loop completed implementation in commit `{sha}` on branch `{branch}`.",
            )
            linear.move_issue_state(issue.id, done_id)

            # Ensure clean state before next ticket.
            ensure_clean_worktree(repo)
            print(f"Done: {issue.identifier} ({sha})")

        except Exception as err:
            msg = f"{issue.identifier}: {err}"
            failures.append(msg)
            print(f"ERROR: {msg}", file=sys.stderr)

            if not args.dry_run:
                try:
                    linear.add_comment(
                        issue.id,
                        "Ralph loop failed for this ticket.\n\n"
                        f"Error:\n```\n{err}\n```",
                    )
                except Exception as comment_err:
                    print(f"WARNING: failed to post failure comment: {comment_err}", file=sys.stderr)

            if not args.continue_on_error:
                break

    if failures:
        print("\nFailures:")
        for item in failures:
            print(f"- {item}")
        return 1

    print("\nRalph loop completed successfully.")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except RalphError as exc:
        print(f"Fatal: {exc}", file=sys.stderr)
        raise SystemExit(1)
