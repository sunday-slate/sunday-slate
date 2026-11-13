#!/usr/bin/env python3
"""One-time ESPN ID backfill from the local nflverse cache; run after migration.

Missing or ambiguous identities are reported without being changed.
"""

from __future__ import annotations

import argparse
import os
import sqlite3
import sys
from collections import defaultdict
from urllib.parse import quote


def open_database(path: str, mode: str) -> sqlite3.Connection:
    if not os.path.isfile(path):
        raise FileNotFoundError(path)
    uri = f"file:{quote(os.path.abspath(path), safe='/')}?mode={mode}"
    connection = sqlite3.connect(uri, uri=True)
    connection.isolation_level = None
    return connection


def normalize(value: object) -> str | None:
    if value is None:
        return None
    text = str(value).strip()
    return text or None


def read_sources(connection: sqlite3.Connection) -> dict[str, str]:
    sources: dict[str, str] = {}
    for gsis_id, espn_id in connection.execute(
        "SELECT gsis_id, espn_id FROM players"
    ):
        gsis = normalize(gsis_id)
        espn = normalize(espn_id)
        if gsis is not None and espn is not None:
            sources[gsis] = espn
    return sources


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Backfill player_links.espn_player_id from cached nflverse identities."
    )
    parser.add_argument("--nfl-data-db", required=True, help="NFL data SQLite file")
    parser.add_argument("--nflverse-db", required=True, help="nflverse SQLite file")
    args = parser.parse_args()

    try:
        nflverse = open_database(args.nflverse_db, "ro")
    except (OSError, sqlite3.Error) as error:
        print(f"error: cannot open nflverse database: {error}", file=sys.stderr)
        return 1
    try:
        sources = read_sources(nflverse)
    except sqlite3.Error as error:
        print(f"error: cannot read nflverse players: {error}", file=sys.stderr)
        return 1
    finally:
        nflverse.close()

    try:
        nfl_data = open_database(args.nfl_data_db, "rw")
    except (OSError, sqlite3.Error) as error:
        print(f"error: cannot open NFL data database: {error}", file=sys.stderr)
        return 1

    try:
        nfl_data.execute("BEGIN IMMEDIATE")
        rows = list(
            nfl_data.execute("SELECT gsis_player_id, espn_player_id FROM player_links")
        )

        already_populated = 0
        missing: list[str] = []
        candidates: list[tuple[str, str]] = []
        for gsis_id, espn_id in rows:
            gsis = normalize(gsis_id)
            if gsis is None:
                continue
            if normalize(espn_id) is not None:
                already_populated += 1
                continue
            source = sources.get(gsis)
            if source is None:
                missing.append(gsis)
            else:
                candidates.append((gsis, source))

        owners: dict[str, str] = {}
        for gsis_id, espn_id in rows:
            gsis = normalize(gsis_id)
            espn = normalize(espn_id)
            if gsis is not None and espn is not None:
                owners.setdefault(espn, gsis)

        claims: dict[str, list[str]] = defaultdict(list)
        for gsis, espn in candidates:
            claims[espn].append(gsis)

        valid: list[tuple[str, str]] = []
        conflicts: list[tuple[str, str, str]] = []
        for gsis, espn in candidates:
            claimants = claims[espn]
            if len(claimants) > 1:
                conflicts.append(
                    (gsis, espn, "claimed by multiple target rows: " + ", ".join(sorted(claimants)))
                )
            elif espn in owners and owners[espn] != gsis:
                conflicts.append((gsis, espn, f"already assigned to {owners[espn]}"))
            else:
                valid.append((gsis, espn))

        updated = 0
        for gsis, espn in valid:
            cursor = nfl_data.execute(
                "UPDATE player_links SET espn_player_id = ? "
                "WHERE gsis_player_id = ? AND espn_player_id IS NULL",
                (espn, gsis),
            )
            updated += cursor.rowcount
        nfl_data.execute("COMMIT")
    except sqlite3.Error as error:
        try:
            nfl_data.execute("ROLLBACK")
        except sqlite3.Error:
            pass
        print(f"error: backfill failed: {error}", file=sys.stderr)
        return 1
    finally:
        nfl_data.close()

    print(f"updated={updated}")
    print(f"missing={len(missing)}")
    print(f"conflicts={len(conflicts)}")
    print(f"already_populated={already_populated}")
    for gsis in sorted(missing):
        print(f"missing gsis={gsis}")
    for gsis, espn, reason in sorted(conflicts):
        print(f"conflict gsis={gsis} espn={espn} reason={reason}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
