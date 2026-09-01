#!/usr/bin/env python3
"""Read-only: open the session from a copy of tdata and read the dialogs with
their access_hash.

Deletes nothing. Writes access_hash into the same state/glisser.db the Rust
side uses: the export does not know it, MTProto does, and without it purge
cannot address a dialog. It doubles as a probe on a live account: if there is
any ban risk, it lands here, on harmless reading.

Run with Telegram Desktop CLOSED and against a copy of tdata, not the live
one: the same auth_key in two processes reads as a duplicate and logs both out.

    .venv/bin/python pull.py
"""
import asyncio
import sqlite3
import sys
from pathlib import Path

# Must be applied before opentele is imported.
import tdata_compat

tdata_compat.apply()

from opentele.td import TDesktop
from opentele.api import UseCurrentSession

STATE = Path(__file__).resolve().parent.parent / "state"
TDATA = STATE / "tdata-copy" / "tdata"
DB = STATE / "glisser.db"
SESSION = STATE / "glisser.session"


async def main():
    if not TDATA.exists():
        sys.exit(f"no {TDATA} — copy Telegram Desktop's tdata folder there first")

    tdesk = TDesktop(str(TDATA))
    if not tdesk.isLoaded():
        sys.exit("tdata did not load — Desktop may have a local passcode set")

    # UseCurrentSession continues the existing Desktop session instead of
    # logging in again: same auth_key, same api_id 2040, no new login.
    client = await tdesk.ToTelethon(str(SESSION), UseCurrentSession)
    await client.connect()

    me = await client.get_me()
    print(f"signed in as: {me.first_name} (id {me.id}, @{me.username})")
    print("read-only pass — nothing is deleted\n")

    db = sqlite3.connect(DB)
    seen = updated = 0
    async for d in client.iter_dialogs():
        e = d.entity
        ah = getattr(e, "access_hash", None)
        seen += 1
        # Only fills in access_hash for dialogs already known from the export;
        # new ones are not created, the corpus comes from the export.
        cur = db.execute(
            "UPDATE dialogs SET access_hash = ?, username = COALESCE(username, ?) "
            "WHERE id = ?",
            (ah, getattr(e, "username", None), e.id),
        )
        if cur.rowcount:
            updated += 1
    db.commit()

    got = db.execute(
        "SELECT COUNT(*) FROM dialogs WHERE access_hash IS NOT NULL"
    ).fetchone()[0]
    total = db.execute("SELECT COUNT(*) FROM dialogs").fetchone()[0]
    print(f"dialogs on the account : {seen}")
    print(f"access_hash updated    : {updated}")
    print(f"in the base with one   : {got} of {total}")
    db.close()
    await client.disconnect()


if __name__ == "__main__":
    asyncio.run(main())
