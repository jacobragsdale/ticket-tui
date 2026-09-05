#!/usr/bin/env python3
"""Fill a ticket-tui database with ~35k work items shaped like a real project.

    python3 scripts/seed_large_db.py bench.sqlite3
    target/release/ticket-tui --database bench.sqlite3 --refresh 0

The schema is read out of src/db.rs, so the file is whatever the build
expects. Every row belongs to the organization `bench`, and sync_meta says so,
which is what makes a running ticket-tui refuse to sync over it: the database
stays offline and stays large. Stdlib only.
"""

import argparse
import datetime as dt
import random
import re
import sqlite3
import sys
from pathlib import Path

ORG = "bench"
PROJECT = "bench"
ME = "Jacob Ragsdale"
PEOPLE = [ME, "Priya Natarajan", "Tomasz Wierzbicki", "Ana Lucía Ferreira",
          "Kwame Mensah", "Ingrid Solberg", "Daniel Okafor", "Mei-Ling Chen"]
AREAS = [f"{PROJECT}\\Payments", f"{PROJECT}\\Ledger", f"{PROJECT}\\Onboarding",
         f"{PROJECT}\\Platform"]
TAGS = ["", "", "inbox", "tech-debt", "security", "customer;urgent", "spike", "blocked"]
WORDS = ("vault token ledger rotate reconcile settle batch retry alert queue schema "
         "migration policy audit sandbox webhook merchant refund dispute limit").split()


def schema() -> tuple[str, int]:
    source = (Path(__file__).resolve().parents[1] / "src" / "db.rs").read_text()
    ddl = re.search(r'const RESET_SCHEMA: &str = r"(.*?)";', source, re.S).group(1)
    version = int(re.search(r"SCHEMA_VERSION: i64 = (\d+)", source).group(1))
    return ddl, version


def sentence(rng: random.Random, words: int) -> str:
    return " ".join(rng.choice(WORDS) for _ in range(words)).capitalize() + "."


def description(rng: random.Random, title: str) -> tuple[str, str]:
    """Roughly 2 KB of the HTML the rich-text editor writes, and its text."""
    paragraphs = [sentence(rng, 28) for _ in range(4)]
    bullets = [sentence(rng, 9) for _ in range(5)]
    html = f"<div><h2>{title}</h2>" + "".join(f"<p>{p}</p>" for p in paragraphs)
    html += "<ul>" + "".join(f"<li>{b}</li>" for b in bullets) + "</ul></div>"
    text = title + "\n\n" + "\n\n".join(paragraphs) + "\n\n" + "\n".join(f"- {b}" for b in bullets)
    return text, html


def stamp(when: dt.datetime) -> str:
    return when.strftime("%Y-%m-%dT%H:%M:%S.000Z")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("database", type=Path)
    parser.add_argument("--epics", type=int, default=60)
    parser.add_argument("--features", type=int, default=10)
    parser.add_argument("--stories", type=int, default=8)
    parser.add_argument("--tasks", type=int, default=6)
    parser.add_argument("--seed", type=int, default=35)
    parser.add_argument("--force", action="store_true", help="overwrite an existing file")
    args = parser.parse_args()
    if args.database.exists() and not args.force:
        print(f"{args.database} exists; pass --force to rebuild it", file=sys.stderr)
        return 2

    rng = random.Random(args.seed)
    now = dt.datetime(2026, 9, 5, 12, tzinfo=dt.timezone.utc)
    ddl, version = schema()
    connection = sqlite3.connect(args.database)
    connection.executescript(ddl)
    connection.execute(f"PRAGMA user_version = {version}")

    rows, relations = [], []
    next_id = 1000

    def add(kind: str, parent: int | None, depth: int) -> int:
        nonlocal next_id
        next_id += 1
        work_item_id = next_id
        title = f"{kind} {sentence(rng, 3 + depth)[:-1]}"
        created = now - dt.timedelta(days=rng.uniform(30, 720))
        changed = created + dt.timedelta(days=rng.uniform(0, (now - created).days or 1))
        # Roughly a third of the work is done, a fifth is in flight.
        state = rng.choices(["New", "Active", "Resolved", "Closed", "Removed"],
                            weights=[40, 20, 5, 32, 3])[0]
        text, html = description(rng, title)
        rows.append((
            ORG, PROJECT, work_item_id, rng.randint(1, 12), kind, title, state,
            None if state in ("New", "Active") else "Completed",
            rng.choice(PEOPLE) if rng.random() < 0.8 else None,
            rng.choice([1, 2, 2, 3, 3, 3, 4]),
            rng.choice(AREAS), f"{PROJECT}\\Sprint {rng.randint(1, 26)}",
            rng.choice(TAGS), text, html, stamp(created), stamp(changed),
            f"https://dev.azure.com/{ORG}/{PROJECT}/_workitems/edit/{work_item_id}", 0,
        ))
        if parent is not None:
            relations.append((ORG, work_item_id, parent, "parent"))
            relations.append((ORG, parent, work_item_id, "child"))
        return work_item_id

    for _ in range(args.epics):
        epic = add("Epic", None, 0)
        for _ in range(args.features):
            feature = add("Feature", epic, 1)
            for _ in range(args.stories):
                story = add("User Story", feature, 2)
                for _ in range(args.tasks):
                    add("Task", story, 3)

    with connection:
        connection.executemany(
            "INSERT INTO work_items (organization, project, work_item_id, revision,"
            " work_item_type, title, state, reason, assigned_to, priority, area_path,"
            " iteration_path, tags, description, description_html, created_at, changed_at,"
            " web_url, details_rev) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            rows,
        )
        connection.executemany(
            "INSERT INTO work_item_relations (organization, from_id, to_id, kind) VALUES (?,?,?,?)",
            relations,
        )
        connection.executemany(
            "INSERT INTO sync_meta (key, value) VALUES (?, ?)",
            [("organization", ORG), ("project", PROJECT), ("me_display_name", ME),
             ("watermark_changed_at", max(row[16] for row in rows))],
        )
    print(f"{len(rows)} work items, {len(relations)} relations -> {args.database}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
