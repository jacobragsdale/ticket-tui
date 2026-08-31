#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""Read and summarize ticket-tui's live agent context.

Schema 3 describes every tab — work items, repos, pull requests, pipelines,
AKS, ACR and Key Vault — whether or not the user is looking at them, so this
prints the active tab first and then whatever each tab holds.

Nothing here prints a secret's value: the context has no field for one.
"""

from __future__ import annotations

import argparse
import json
import os
import sqlite3
import sys
from pathlib import Path
from typing import cast

SCHEMA_VERSION = 3


def default_database_path() -> Path:
    if sys.platform == "darwin":
        return Path.home() / "Library/Application Support/ticket-tui/tickets.sqlite3"
    configured = os.environ.get("XDG_DATA_HOME")
    data_home = Path(configured) if configured else Path.home() / ".local/share"
    return data_home / "ticket-tui/tickets.sqlite3"


def context_path_for(database: Path) -> Path:
    return database.with_name(f"{database.stem}.context.json")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    source = parser.add_mutually_exclusive_group()
    source.add_argument("--database", type=Path, help="ticket-tui SQLite path")
    source.add_argument("--context", type=Path, help="context JSON path")
    output = parser.add_mutually_exclusive_group()
    output.add_argument("--json", action="store_true", help="print raw JSON")
    output.add_argument(
        "--details",
        action="store_true",
        help="also read full selected-ticket records from SQLite",
    )
    return parser.parse_args()


def process_is_live(process_id: object) -> bool:
    if not isinstance(process_id, int) or process_id <= 0:
        return False
    try:
        os.kill(process_id, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def object_mapping(value: object) -> dict[str, object]:
    if not isinstance(value, dict) or not all(isinstance(key, str) for key in value):
        raise TypeError("expected a JSON object with string keys")
    return cast(dict[str, object], value)


def object_list(value: object) -> list[object]:
    if not isinstance(value, list):
        raise TypeError("expected a JSON array")
    return cast(list[object], value)


def integer(value: object, field: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int):
        raise TypeError(f"expected {field} to be an integer")
    return value


def text(value: object, field: str) -> str:
    if not isinstance(value, str):
        raise TypeError(f"expected {field} to be a string")
    return value


def boolean(value: object, field: str) -> bool:
    if not isinstance(value, bool):
        raise TypeError(f"expected {field} to be a boolean")
    return value


def validate_ticket(value: object, field: str) -> None:
    ticket = object_mapping(value)
    for name in (
        "organization",
        "project",
        "work_item_type",
        "title",
        "state",
        "web_url",
    ):
        text(ticket.get(name), f"{field}.{name}")
    integer(ticket.get("id"), f"{field}.id")
    assigned_to = ticket.get("assigned_to")
    if assigned_to is not None:
        text(assigned_to, f"{field}.assigned_to")
    priority = ticket.get("priority")
    if priority is not None:
        integer(priority, f"{field}.priority")
    tags = object_list(ticket.get("tags"))
    for index, tag in enumerate(tags):
        text(tag, f"{field}.tags[{index}]")
    boolean(ticket.get("bookmarked"), f"{field}.bookmarked")
    boolean(ticket.get("checked"), f"{field}.checked")


def validate_sync(value: object) -> None:
    sync = object_mapping(value)
    for name in ("organization", "project", "last_success_at", "last_error"):
        field = sync.get(name)
        if field is not None:
            text(field, f"sync.{name}")
    integer(sync.get("refresh_seconds"), "sync.refresh_seconds")
    boolean(sync.get("in_progress"), "sync.in_progress")
    boolean(sync.get("offline"), "sync.offline")


def validate_pending_edit(value: object, field: str) -> None:
    edit = object_mapping(value)
    integer(edit.get("id"), f"{field}.id")
    for name in ("field", "value", "since"):
        text(edit.get(name), f"{field}.{name}")


def validate_context(data: dict[str, object]) -> None:
    integer(data.get("schema_version"), "schema_version")
    integer(data.get("process_id"), "process_id")
    for name in ("updated_at", "database_path", "active_tab"):
        text(data.get(name), name)
    me = data.get("me")
    if me is not None:
        text(me, "me")

    validate_sync(data.get("sync"))
    pending_edits = object_list(data.get("pending_edits"))
    for index, value in enumerate(pending_edits):
        validate_pending_edit(value, f"pending_edits[{index}]")

    validate_work_items(object_mapping(data.get("work_items")))
    # The other six tabs are shapes this script reads rather than depends on,
    # so they are checked for their containers and left otherwise open: a field
    # added to one of them should not stop an agent reading the rest.
    repos = object_mapping(data.get("repos"))
    object_list(repos.get("visible_rows"))
    pull_requests = object_mapping(data.get("pull_requests"))
    object_list(pull_requests.get("visible_rows"))
    integer(pull_requests.get("to_review_count"), "pull_requests.to_review_count")
    pipelines = object_mapping(data.get("pipelines"))
    text(pipelines.get("level"), "pipelines.level")
    object_list(pipelines.get("watched"))
    aks = object_mapping(data.get("aks"))
    integer(aks.get("visible_rows"), "aks.visible_rows")
    integer(aks.get("unhealthy"), "aks.unhealthy")
    acr = object_mapping(data.get("acr"))
    text(acr.get("level"), "acr.level")
    integer(acr.get("visible_rows"), "acr.visible_rows")
    key_vault = object_mapping(data.get("key_vault"))
    text(key_vault.get("level"), "key_vault.level")
    integer(key_vault.get("visible_rows"), "key_vault.visible_rows")
    integer(key_vault.get("expiring_certificates"), "key_vault.expiring_certificates")
    arm = object_mapping(data.get("arm"))
    boolean(arm.get("offline"), "arm.offline")
    for name in ("subscription", "last_error"):
        field = arm.get(name)
        if field is not None:
            text(field, f"arm.{name}")


def validate_work_items(data: dict[str, object]) -> None:
    for name in ("mode", "focus", "screen"):
        text(data.get(name), f"work_items.{name}")
    active_view = data.get("active_view")
    if active_view is not None:
        text(active_view, "work_items.active_view")

    search = object_mapping(data.get("search"))
    for name in ("query", "fuzzy_text", "order"):
        text(search.get(name), f"work_items.search.{name}")
    boolean(search.get("pending"), "work_items.search.pending")
    filters = object_list(search.get("filters"))
    for index, value in enumerate(filters):
        text(value, f"work_items.search.filters[{index}]")

    sort = object_mapping(data.get("sort"))
    for name in ("field", "direction", "row_density"):
        text(sort.get(name), f"work_items.sort.{name}")

    tickets = object_mapping(data.get("tickets"))
    for name in ("total_count", "matching_count", "viewport_start", "viewport_size"):
        integer(tickets.get(name), f"work_items.tickets.{name}")
    boolean(tickets.get("finished_hidden"), "work_items.tickets.finished_hidden")
    visible_rows = object_list(tickets.get("visible_rows"))
    for index, value in enumerate(visible_rows):
        validate_ticket(value, f"work_items.tickets.visible_rows[{index}]")

    selected = data.get("selected_ticket")
    if selected is not None:
        validate_ticket(selected, "work_items.selected_ticket")
    checked = object_list(data.get("checked_tickets"))
    for index, value in enumerate(checked):
        validate_ticket(value, f"work_items.checked_tickets[{index}]")

    family_cursor = data.get("family_cursor")
    if family_cursor is not None:
        cursor = object_mapping(family_cursor)
        text(cursor.get("organization"), "work_items.family_cursor.organization")
        integer(cursor.get("id"), "work_items.family_cursor.id")
    integer(data.get("details_scroll_line"), "work_items.details_scroll_line")


def ticket_label(value: object) -> str:
    if value is None:
        return "none"
    ticket = object_mapping(value)
    identity = f"{ticket.get('organization', '?')}#{ticket.get('id', '?')}"
    title = ticket.get("title")
    state = ticket.get("state")
    suffix = " · ".join(str(value) for value in (state, title) if value)
    return f"{identity} · {suffix}" if suffix else identity


def sync_label(sync: dict[str, object]) -> str:
    project = "/".join(
        str(sync[name]) for name in ("organization", "project") if sync.get(name)
    )
    if sync.get("offline"):
        state = "offline"
    elif sync.get("in_progress"):
        state = "pull in progress"
    elif sync.get("last_error"):
        state = f"last pull failed: {sync['last_error']}"
    else:
        state = "ok"
    refresh = sync.get("refresh_seconds") or 0
    timer = f"every {refresh}s" if refresh else "on request"
    last = sync.get("last_success_at") or "never this run"
    return " · ".join(part for part in (project, timer, state, f"synced {last}") if part)


def print_summary(context_path: Path, data: dict[str, object]) -> None:
    live = process_is_live(data.get("process_id"))
    status = "live" if live else "stale process"
    print(f"ticket-tui context: {status}")
    print(f"Context: {context_path}")
    print(f"Database: {data.get('database_path', 'unknown')}")
    print(f"Updated: {data.get('updated_at', 'unknown')}")
    print(f"Signed in as: {data.get('me') or '(unknown; @me will not resolve)'}")
    print(f"Active tab: {data.get('active_tab', '?')}")
    print(f"Sync: {sync_label(object_mapping(data.get('sync')))}")

    pending_edits = object_list(data.get("pending_edits"))
    if pending_edits:
        print(f"Pending edits ({len(pending_edits)}), not answered yet:")
        for value in pending_edits:
            edit = object_mapping(value)
            print(
                f"  - #{edit.get('id', '?')} {edit.get('field', '?')} -> "
                f"{edit.get('value', '?')} (sent {edit.get('since', '?')})"
            )

    print_work_items(object_mapping(data.get("work_items")))
    print_repos(object_mapping(data.get("repos")))
    print_pull_requests(object_mapping(data.get("pull_requests")))
    print_pipelines(object_mapping(data.get("pipelines")))
    print_aks(object_mapping(data.get("aks")))
    print_arm(object_mapping(data.get("arm")))
    print_acr(object_mapping(data.get("acr")))
    print_key_vault(object_mapping(data.get("key_vault")))

    if not live:
        print(
            "Warning: treat this as the last observed view, not current live state.",
            file=sys.stderr,
        )
    sync = object_mapping(data.get("sync"))
    if sync.get("offline") or sync.get("last_error"):
        print(
            "Warning: the rows are the last synced values, not live Azure DevOps state.",
            file=sys.stderr,
        )


def print_work_items(data: dict[str, object]) -> None:
    print("")
    print("[work items]")
    print(
        "View: "
        f"mode={data.get('mode', '?')} "
        f"focus={data.get('focus', '?')} "
        f"screen={data.get('screen', '?')}"
    )
    if data.get("active_view"):
        print(f"Named view: {data['active_view']}")

    search = object_mapping(data.get("search"))
    print(f"Query: {search.get('query') or '(none)'}")
    print(f"Fuzzy: {search.get('fuzzy_text') or '(none)'}")
    filters = object_list(search.get("filters"))
    print(f"Filters: {', '.join(map(str, filters)) if filters else '(none)'}")
    pending = " (search pending)" if search.get("pending") else ""
    print(f"Search order: {search.get('order', '?')}{pending}")

    sort = object_mapping(data.get("sort"))
    print(
        "Sort: "
        f"{sort.get('field', '?')} {sort.get('direction', '?')} "
        f"· density={sort.get('row_density', '?')}"
    )

    tickets = object_mapping(data.get("tickets"))
    start = integer(tickets.get("viewport_start"), "tickets.viewport_start")
    rows = object_list(tickets.get("visible_rows"))
    end = start + len(rows)
    viewport = f"{start + 1}-{end}" if rows else "empty"
    finished = " · finished hidden" if tickets.get("finished_hidden") else ""
    print(
        "Results: "
        f"{tickets.get('matching_count', 0)}/{tickets.get('total_count', 0)} matching "
        f"· viewport={viewport}{finished}"
    )
    selected_value = data.get("selected_ticket")
    print(f"Selected: {ticket_label(selected_value)}")
    if selected_value is not None:
        related = object_list(object_mapping(selected_value).get("related") or [])
        if related:
            print(f"Related ({len(related)}):")
            for value in related:
                link = object_mapping(value)
                held = "" if link.get("in_database") else " (not in this database)"
                print(
                    f"  - {link.get('kind', '?')} {link.get('target', '?')}"
                    f"{' in ' + str(link['repo']) if link.get('repo') else ''}{held}"
                )

    checked = object_list(data.get("checked_tickets"))
    print(f"Checked ({len(checked)}):")
    for ticket in checked:
        print(f"  - {ticket_label(ticket)}")
    if not checked:
        print("  (none)")

    selected = object_mapping(selected_value) if selected_value is not None else {}
    print(f"Visible rows ({len(rows)}):")
    for value in rows:
        ticket = object_mapping(value)
        marker = (
            ">"
            if (
                ticket.get("organization"),
                ticket.get("id"),
            )
            == (selected.get("organization"), selected.get("id"))
            else " "
        )
        checked_marker = "x" if ticket.get("checked") else " "
        print(f" {marker} [{checked_marker}] {ticket_label(ticket)}")
    if not rows:
        print("  (none)")


def local_label(value: object) -> str:
    if value is None:
        return "not on this machine"
    local = object_mapping(value)
    state = str(local.get("branch", "?"))
    if local.get("dirty"):
        state += " dirty"
    for name, sign in (("ahead", "+"), ("behind", "-")):
        count = local.get(name) or 0
        if isinstance(count, int) and count:
            state += f" {sign}{count}"
    if local.get("busy"):
        state += f" ({local['busy']})"
    return f"{state} at {local.get('path', '?')}"


def print_repos(data: dict[str, object]) -> None:
    rows = object_list(data.get("visible_rows"))
    print("")
    print(f"[repos] workspace={data.get('workspace') or '(none)'}")
    selected = data.get("selected")
    name = object_mapping(selected).get("name") if selected is not None else None
    for value in rows:
        repo = object_mapping(value)
        marker = ">" if repo.get("name") == name else " "
        print(
            f" {marker} {repo.get('name', '?')} · {repo.get('default_branch', '?')} · "
            f"{repo.get('pull_requests', 0)} prs · {repo.get('pipelines', 0)} pipelines · "
            f"{local_label(repo.get('local'))}"
        )
    if not rows:
        print("  (none)")


def print_pull_requests(data: dict[str, object]) -> None:
    rows = object_list(data.get("visible_rows"))
    print("")
    print(
        f"[pull requests] {data.get('to_review_count', 0)} waiting on your vote"
        f"{' · closed shown' if data.get('closed_shown') else ''}"
    )
    selected = data.get("selected")
    chosen = object_mapping(selected).get("id") if selected is not None else None
    for value in rows:
        request = object_mapping(value)
        marker = ">" if request.get("id") == chosen else " "
        print(
            f" {marker} !{request.get('id', '?')} {request.get('status', '?')} · "
            f"{request.get('repo', '?')} · {request.get('source_branch', '?')} -> "
            f"{request.get('target_branch', '?')} · {request.get('title', '')}"
        )
    if not rows:
        print("  (none)")
    if selected is not None:
        request = object_mapping(selected)
        reviewers = object_list(request.get("reviewers") or [])
        votes = ", ".join(
            f"{object_mapping(reviewer).get('name', '?')} {object_mapping(reviewer).get('vote', 0)}"
            for reviewer in reviewers
        )
        print(f"  Reviewers: {votes or '(nobody asked)'}")
        print(
            f"  Threads: {request.get('thread_count', 0)}"
            f" ({request.get('unresolved_threads', 0)} unresolved)"
        )


def print_pipelines(data: dict[str, object]) -> None:
    print("")
    print(
        f"[pipelines] level={data.get('level', '?')} · "
        f"{data.get('running', 0)} running · "
        f"{data.get('pending_approvals', 0)} approvals pending"
    )
    pipeline = data.get("selected_pipeline")
    if pipeline is not None:
        chosen = object_mapping(pipeline)
        print(f"  Pipeline: {chosen.get('name', '?')} ({chosen.get('id', '?')})")
    run = data.get("selected_run")
    if run is not None:
        chosen = object_mapping(run)
        print(
            f"  Run: {chosen.get('id', '?')} {chosen.get('status', '?')}"
            f"{' ' + str(chosen['result']) if chosen.get('result') else ''} · "
            f"{chosen.get('build_number', '?')} on {chosen.get('branch', '?')}"
        )
        stages = object_list(chosen.get("stages") or [])
        for value in stages:
            stage = object_mapping(value)
            print(f"    {stage.get('state', '?')} {stage.get('name', '?')}")
    log = data.get("following_log")
    if log is not None:
        tail = object_mapping(log)
        print(
            f"  Log: {tail.get('node', '?')} · {tail.get('line_count', 0)} lines"
            f"{' · following' if tail.get('following') else ' · scrolled'}"
        )
    watched = object_list(data.get("watched"))
    if watched:
        print(f"  Watching: {', '.join(str(run) for run in watched)}")


def print_aks(data: dict[str, object]) -> None:
    """The AKS tab. Pods are read live through kubectl and never stored, so this
    is the last read rather than the cluster's state right now."""
    print("")
    clusters = [str(value) for value in object_list(data.get("clusters") or [])]
    print(
        f"[aks] {', '.join(clusters) or 'no clusters in config.toml'} · "
        f"{data.get('visible_rows', 0)} pods · "
        f"{data.get('unhealthy', 0)} unhealthy"
    )
    pod = data.get("selected")
    if pod is not None:
        chosen = object_mapping(pod)
        print(
            f"  Pod: {chosen.get('cluster', '?')}/{chosen.get('namespace', '?')}/"
            f"{chosen.get('name', '?')} · {chosen.get('status', '?')} · "
            f"{chosen.get('ready', '?')} ready · "
            f"{chosen.get('restarts', 0)} restarts"
        )
        owner = chosen.get("owner")
        if owner:
            print(f"    Owner: {owner}")
        repo = chosen.get("repo")
        if repo:
            print(f"    Repository: {repo}")
        for value in object_list(chosen.get("containers") or []):
            container = object_mapping(value)
            print(
                f"    {container.get('name', '?')}  {container.get('state', '?')}"
                f"  {container.get('image', '?')}"
            )
    log = data.get("following_log")
    if log is not None:
        tail = object_mapping(log)
        print(
            f"  Log: {tail.get('pod', '?')}"
            f"{' -c ' + str(tail['container']) if tail.get('container') else ''}"
            f"{' (previous)' if tail.get('previous') else ''} · "
            f"{tail.get('line_count', 0)} lines"
            f"{' · following' if tail.get('following') else ' · scrolled'}"
        )
    for message in object_list(data.get("errors") or []):
        print(f"  ! {message}")


def print_arm(data: dict[str, object]) -> None:
    """What the two subscription tabs can reach at all. A run with no
    subscription draws both of them empty, and this is the line that says why."""
    print("")
    subscription = data.get("subscription") or "(none resolved)"
    state = "offline" if data.get("offline") else "ok"
    print(f"[arm] subscription={subscription} · {state}")
    if data.get("last_error"):
        print(f"  ! {data['last_error']}")


def print_acr(data: dict[str, object]) -> None:
    """The ACR tab. Registries are read live through Resource Manager and never
    stored, so this is the last read rather than the subscription right now."""
    print("")
    print(
        f"[acr] level={data.get('level') or '?'} · "
        f"{data.get('visible_rows', 0)} rows"
    )
    registry = data.get("selected_registry")
    if registry is not None:
        chosen = object_mapping(registry)
        print(
            f"  Registry: {chosen.get('name', '?')} · {chosen.get('sku', '?')} · "
            f"{chosen.get('resource_group', '?')}/{chosen.get('location', '?')} · "
            f"{chosen.get('login_server', '?')}"
        )
    repository = data.get("selected_repository")
    if repository is not None:
        chosen = object_mapping(repository)
        tags = chosen.get("tags")
        print(
            f"  Repository: {chosen.get('name', '?')} · "
            f"{'—' if tags is None else tags} tags · "
            f"updated {chosen.get('updated') or '—'}"
        )
    tag = data.get("selected_tag")
    if tag is not None:
        chosen = object_mapping(tag)
        print(
            f"  Tag: {chosen.get('name', '?')} · {chosen.get('digest', '?')} · "
            f"created {chosen.get('created') or '—'}"
        )


def print_key_vault(data: dict[str, object]) -> None:
    """The Key Vault tab. `revealed` says a value is on the user's screen this
    minute; the value itself is not in the document and never will be."""
    print("")
    print(
        f"[key vault] level={data.get('level') or '?'} · "
        f"{data.get('visible_rows', 0)} rows · "
        f"{data.get('expiring_certificates', 0)} certificates expiring"
    )
    vault = data.get("selected_vault")
    if vault is not None:
        chosen = object_mapping(vault)
        print(
            f"  Vault: {chosen.get('name', '?')} · {chosen.get('sku', '?')} · "
            f"{chosen.get('resource_group', '?')}/{chosen.get('location', '?')} · "
            f"{chosen.get('uri', '?')}"
        )
    item = data.get("selected_item")
    if item is not None:
        chosen = object_mapping(item)
        print(
            f"  Item: {chosen.get('kind', '?')} {chosen.get('name', '?')} · "
            f"{'enabled' if chosen.get('enabled') else 'disabled'} · "
            f"updated {chosen.get('updated') or '—'} · "
            f"expires {chosen.get('expires') or 'never'}"
        )
        if chosen.get("revealed"):
            print("    value showing on screen (not in this document)")


def print_selected_details(database: Path, selected_value: object) -> None:
    if selected_value is None:
        print("\nSelected ticket details: none")
        return
    selected = object_mapping(selected_value)
    connection = sqlite3.connect(f"{database.resolve().as_uri()}?mode=ro", uri=True)
    connection.row_factory = sqlite3.Row
    key = (selected["organization"], selected["id"])
    try:
        ticket = connection.execute(
            "SELECT * FROM work_items WHERE organization = ? AND work_item_id = ?",
            key,
        ).fetchone()
        if ticket is None:
            print("\nSelected ticket details: ticket is absent from SQLite")
            return
        print("\nSelected ticket details:")
        # description_html is the raw Azure DevOps markup kept for round-trip
        # editing; the flattened description column is the readable one.
        fields: list[str] = [name for name in ticket.keys() if name != "description_html"]
        for field in fields:
            print(f"  {field}: {ticket[field]}")

        relations = connection.execute(
            "SELECT from_id, to_id, kind FROM work_item_relations "
            "WHERE organization = ? AND (from_id = ? OR to_id = ?) "
            "ORDER BY from_id, to_id, kind",
            (key[0], key[1], key[1]),
        ).fetchall()
        print(f"Relations ({len(relations)}):")
        for relation in relations:
            print(f"  {relation['from_id']} -{relation['kind']}-> {relation['to_id']}")

        comments = connection.execute(
            "SELECT created_at, author, body FROM work_item_comments "
            "WHERE organization = ? AND work_item_id = ? ORDER BY created_at",
            key,
        ).fetchall()
        print(f"Comments ({len(comments)}):")
        for comment in comments:
            print(
                f"  {comment['created_at']} · {comment['author'] or 'unknown'}: {comment['body']}"
            )

        history = connection.execute(
            "SELECT revision, changed_at, changed_by, field_name, old_value, new_value "
            "FROM work_item_history WHERE organization = ? AND work_item_id = ? "
            "ORDER BY revision, field_name",
            key,
        ).fetchall()
        print(f"History ({len(history)}):")
        for entry in history:
            print(
                f"  r{entry['revision']} · {entry['changed_at']} · "
                f"{entry['field_name']}: {entry['old_value']} -> {entry['new_value']} "
                f"({entry['changed_by'] or 'unknown'})"
            )
    finally:
        connection.close()


def main() -> int:
    args = parse_args()
    database = args.database or default_database_path()
    context_path = args.context or context_path_for(database)
    try:
        raw: object = json.loads(context_path.read_text(encoding="utf-8"))
        data = object_mapping(raw)
    except FileNotFoundError:
        print(f"No live ticket-tui context at {context_path}", file=sys.stderr)
        print(
            "No ticket-tui is running, or it was started with --database: pass "
            "the same path here. Read the backlog with `ticket-tui list` instead.",
            file=sys.stderr,
        )
        return 2
    except (OSError, json.JSONDecodeError, TypeError) as error:
        print(f"Could not read {context_path}: {error}", file=sys.stderr)
        return 1

    if data.get("schema_version") != SCHEMA_VERSION:
        print(
            f"Unsupported context schema {data.get('schema_version')!r}; "
            f"expected {SCHEMA_VERSION}",
            file=sys.stderr,
        )
        return 1
    try:
        validate_context(data)
    except TypeError as error:
        print(f"Invalid context in {context_path}: {error}", file=sys.stderr)
        return 1
    if args.json:
        print(json.dumps(data, indent=2, ensure_ascii=False))
        return 0

    print_summary(context_path, data)
    if args.details:
        database_value = data.get("database_path")
        if not isinstance(database_value, str):
            print("Context database_path is not a string", file=sys.stderr)
            return 1
        database = Path(database_value)
        try:
            work_items = object_mapping(data.get("work_items"))
            print_selected_details(database, work_items.get("selected_ticket"))
        except (OSError, sqlite3.Error, KeyError) as error:
            print(f"Could not read selected ticket details: {error}", file=sys.stderr)
            return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
