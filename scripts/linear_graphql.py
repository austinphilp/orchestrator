#!/usr/bin/env python3
"""Run authenticated GraphQL requests against Linear.

This repo-local helper exists so agents can use `scripts/linear_graphql.py`
from any worktree without depending on external skill paths.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import urllib.error
import urllib.request


DEFAULT_ENDPOINT = "https://api.linear.app/graphql"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Execute a GraphQL query against Linear."
    )
    query_group = parser.add_mutually_exclusive_group()
    query_group.add_argument("--query", help="Inline GraphQL query or mutation.")
    query_group.add_argument(
        "--query-file", help="Path to a file containing the GraphQL operation."
    )
    parser.add_argument(
        "--variables",
        help="Variables object as JSON string. Example: '{\"id\":\"AP-123\"}'",
    )
    parser.add_argument("--variables-file", help="Path to JSON file with variables.")
    parser.add_argument(
        "--operation-name",
        help="Optional GraphQL operationName when the document contains multiple ops.",
    )
    parser.add_argument(
        "--endpoint",
        default=os.getenv("ORCHESTRATOR_LINEAR_API_URL", DEFAULT_ENDPOINT),
        help="GraphQL endpoint URL.",
    )
    parser.add_argument(
        "--token",
        default=os.getenv("LINEAR_API_KEY"),
        help="Linear API key. Defaults to LINEAR_API_KEY.",
    )
    parser.add_argument(
        "--allow-errors",
        action="store_true",
        help="Exit zero even when GraphQL 'errors' is present.",
    )
    parser.add_argument(
        "--compact",
        action="store_true",
        help="Print compact JSON instead of pretty JSON.",
    )
    parser.add_argument(
        "--show-rate-limit",
        action="store_true",
        help="Print Linear rate-limit headers to stderr.",
    )
    args = parser.parse_args()

    if args.variables and args.variables_file:
        parser.error("Use either --variables or --variables-file, not both.")

    return args


def read_query(args: argparse.Namespace) -> str:
    if args.query:
        return args.query
    if args.query_file:
        with open(args.query_file, "r", encoding="utf-8") as handle:
            return handle.read()
    if sys.stdin.isatty():
        raise ValueError(
            "No query provided. Use --query, --query-file, or pipe a query via stdin."
        )
    return sys.stdin.read()


def read_variables(args: argparse.Namespace) -> dict | None:
    if args.variables_file:
        with open(args.variables_file, "r", encoding="utf-8") as handle:
            return json.load(handle)
    if args.variables:
        return json.loads(args.variables)
    return None


def run_request(
    endpoint: str, token: str, payload: dict, show_rate_limit: bool
) -> tuple[int, dict, str]:
    body = json.dumps(payload).encode("utf-8")
    request = urllib.request.Request(
        endpoint,
        data=body,
        headers={
            "Authorization": token,
            "Content-Type": "application/json",
        },
        method="POST",
    )

    status = 0
    headers: dict = {}
    raw_text = ""
    try:
        with urllib.request.urlopen(request) as response:
            status = response.status
            headers = dict(response.headers.items())
            raw_text = response.read().decode("utf-8")
    except urllib.error.HTTPError as error:
        status = error.code
        headers = dict(error.headers.items()) if error.headers else {}
        raw_text = error.read().decode("utf-8", errors="replace")

    if show_rate_limit:
        for name, value in headers.items():
            lower = name.lower()
            if lower.startswith("x-ratelimit") or lower.startswith("x-complexity"):
                print(f"{name}: {value}", file=sys.stderr)

    return status, headers, raw_text


def print_json(text: str, compact: bool) -> dict | None:
    try:
        parsed = json.loads(text)
    except json.JSONDecodeError:
        print(text)
        return None

    if compact:
        print(json.dumps(parsed, separators=(",", ":")))
    else:
        print(json.dumps(parsed, indent=2, sort_keys=True))
    return parsed


def main() -> int:
    args = parse_args()
    if not args.token:
        print("Missing API token. Set LINEAR_API_KEY or pass --token.", file=sys.stderr)
        return 2

    try:
        query = read_query(args).strip()
        variables = read_variables(args)
    except (ValueError, json.JSONDecodeError) as error:
        print(f"Input error: {error}", file=sys.stderr)
        return 2

    payload: dict = {"query": query}
    if variables is not None:
        payload["variables"] = variables
    if args.operation_name:
        payload["operationName"] = args.operation_name

    status, _, raw_text = run_request(
        endpoint=args.endpoint,
        token=args.token,
        payload=payload,
        show_rate_limit=args.show_rate_limit,
    )
    response_data = print_json(raw_text, args.compact)

    if status >= 400:
        return 1
    if (
        isinstance(response_data, dict)
        and response_data.get("errors")
        and not args.allow_errors
    ):
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
