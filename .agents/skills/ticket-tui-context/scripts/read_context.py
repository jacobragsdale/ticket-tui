#!/usr/bin/env python3
# /// script
# requires-python = ">=3.11"
# ///
"""Read and summarize ticket-tui's live agent context."""

from __future__ import annotations

import argparse
import json
import os
import sqlite3
import sys
from pathlib import Path
from typing import cast

SCHEMA_VERSION = 1


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


def validate_context(data: dict[str, object]) -> None:
    integer(data.get("schema_version"), "schema_version")
    integer(data.get("process_id"), "process_id")
    for name in ("updated_at", "database_path", "mode", "focus", "screen"):
        text(data.get(name), name)
    active_view = data.get("active_view")
    if active_view is not None:
        text(active_view, "active_view")

    search = object_mapping(data.get("search"))
    for name in ("query", "fuzzy_text", "order"):
        text(search.get(name), f"search.{name}")
    boolean(search.get("pending"), "search.pending")
    filters = object_list(search.get("filters"))
    for index, value in enumerate(filters):
        text(value, f"search.filters[{index}]")

    sort = object_mapping(data.get("sort"))
    for name in ("field", "direction", "row_density"):
        text(sort.get(name), f"sort.{name}")

    tickets = object_mapping(data.get("tickets"))
    for name in ("total_count", "matching_count", "viewport_start", "viewport_size"):
        integer(tickets.get(name), f"tickets.{name}")
    visible_rows = object_list(tickets.get("visible_rows"))
    for index, value in enumerate(visible_rows):
        validate_ticket(value, f"tickets.visible_rows[{index}]")

    selected = data.get("selected_ticket")
    if selected is not None:
        validate_ticket(selected, "selected_ticket")
    checked = object_list(data.get("checked_tickets"))
    for index, value in enumerate(checked):
        validate_ticket(value, f"checked_tickets[{index}]")

    family_cursor = data.get("family_cursor")
    if family_cursor is not None:
        cursor = object_mapping(family_cursor)
        text(cursor.get("organization"), "family_cursor.organization")
        integer(cursor.get("id"), "family_cursor.id")
    integer(data.get("details_scroll_line"), "details_scroll_line")


def ticket_label(value: object) -> str:
    if value is None:
        return "none"
    ticket = object_mapping(value)
    identity = f"{ticket.get('organization', '?')}#{ticket.get('id', '?')}"
    title = ticket.get("title")
    state = ticket.get("state")
    suffix = " · ".join(str(value) for value in (state, title) if value)
    return f"{identity} · {suffix}" if suffix else identity


def print_summary(context_path: Path, data: dict[str, object]) -> None:
    live = process_is_live(data.get("process_id"))
    status = "live" if live else "stale process"
    print(f"ticket-tui context: {status}")
    print(f"Context: {context_path}")
    print(f"Updated: {data.get('updated_at', 'unknown')}")
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
    print(
        "Results: "
        f"{tickets.get('matching_count', 0)}/{tickets.get('total_count', 0)} matching "
        f"· viewport={viewport}"
    )
    print(f"Selected: {ticket_label(data.get('selected_ticket'))}")

    checked = object_list(data.get("checked_tickets"))
    print(f"Checked ({len(checked)}):")
    for ticket in checked:
        print(f"  - {ticket_label(ticket)}")
    if not checked:
        print("  (none)")

    selected_value = data.get("selected_ticket")
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

    if not live:
        print(
            "Warning: treat this as the last observed view, not current live state.",
            file=sys.stderr,
        )


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
        fields: list[str] = ticket.keys()
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
        print("If --database is in use, pass that database path.", file=sys.stderr)
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
            print_selected_details(database, data.get("selected_ticket"))
        except (OSError, sqlite3.Error, KeyError) as error:
            print(f"Could not read selected ticket details: {error}", file=sys.stderr)
            return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
