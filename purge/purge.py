#!/usr/bin/env python3
"""Deletion by plan, at a human pace.

Reads `plan` (only the rows with approved = 1) and deletes through the same
Desktop session pull.py uses. The unit of work is a connected stretch of
conversation, not a single line: a person selects a stretch with the mouse and
presses delete, and the traffic should look the same. Hence the pauses — read,
select, delete, move on.

Order of writes matters more than speed: the row in `deletions` appears BEFORE
the API call. If the process dies exactly on the call, the trace remains and a
repeat run will not blindly hit the same messages again.

Dry run by default: prints what it would delete and does NOT touch the
network. It deletes only with --yes.

Run with Telegram Desktop CLOSED and against a copy of tdata: the same
auth_key in two processes reads as a duplicate and logs both out.
"""
import argparse
import asyncio
import random
import sqlite3
import sys
import time
from pathlib import Path

STATE = Path(__file__).resolve().parent.parent / "state"
TDATA = STATE / "tdata-copy" / "tdata"
DB = STATE / "glisser.db"
SESSION = STATE / "glisser.session"

# Telegram's limit for one messages.deleteMessages call, and the upper bound
# of a chunk: no one selects more than a hundred messages with a mouse.
MAX_RANGE = 100

# Pauses, in seconds, drawn uniformly from a span — a fixed interval would be
# a signature of a machine by itself.
READ_PER_MSG = (0.25, 0.7)      # re-reading what is about to be deleted
BETWEEN_RANGES = (15, 45)       # between chunks inside one dialog
BETWEEN_DIALOGS = (90, 180)     # between dialogs: found it, opened it, read

# Nobody goes through their correspondence for six hours straight. Every few
# dozen chunks, step away: tea, a call, another window. Not cosmetic — an even
# stream of requests a working day long is exactly the shape Telegram counts.
BREAK_EVERY = (60, 90)          # chunks between breaks
BREAK_FOR = (300, 600)          # the break itself


def pause(span: tuple[float, float]) -> float:
    return random.uniform(*span)


def ranges(db: sqlite3.Connection, dialog_id: int, doomed: list[int]) -> list[list[int]]:
    """Split the doomed messages into connected stretches of conversation.

    Adjacency is measured by position in the dialog, not by the difference of
    ids: ids come with gaps, and arithmetic over them would glue together
    chunks with half a page of surviving text between them on screen.
    """
    order = [r[0] for r in db.execute(
        "SELECT msg_id FROM messages WHERE dialog_id = ? ORDER BY msg_id", (dialog_id,)
    )]
    pos = {m: i for i, m in enumerate(order)}
    known = [m for m in doomed if m in pos]

    out: list[list[int]] = []
    cur: list[int] = []
    for m in sorted(known, key=lambda m: pos[m]):
        if cur and pos[m] == pos[cur[-1]] + 1 and len(cur) < MAX_RANGE:
            cur.append(m)
        else:
            if cur:
                out.append(cur)
            cur = [m]
    if cur:
        out.append(cur)
    return out


def load(db: sqlite3.Connection, only: list[int] | None):
    """The plan, grouped by dialog.

    `done` and `pending` are left out: the first is finished, the second ended
    who knows how and is sorted out by hand. `error` is included — otherwise a
    chunk that failed on a network glitch would silently drop out forever.
    """
    rows = db.execute(
        """
        SELECT p.dialog_id, p.msg_id, d.kind, d.access_hash, d.title, m.date
        FROM plan p
        JOIN dialogs d ON d.id = p.dialog_id
        JOIN messages m ON m.dialog_id = p.dialog_id AND m.msg_id = p.msg_id
        LEFT JOIN deletions x
               ON x.dialog_id = p.dialog_id AND x.msg_id = p.msg_id
        WHERE p.approved = 1
          AND (x.msg_id IS NULL OR x.state = 'error')
        ORDER BY p.dialog_id, p.msg_id
        """
    ).fetchall()

    by_dialog: dict[int, dict] = {}
    for dialog_id, msg_id, kind, access_hash, title, date in rows:
        if only and dialog_id not in only:
            continue
        d = by_dialog.setdefault(dialog_id, {
            "kind": kind, "access_hash": access_hash, "title": title,
            "msgs": [], "oldest": date,
        })
        d["msgs"].append(msg_id)
        d["oldest"] = min(d["oldest"], date)
    return by_dialog


def peer_of(kind: str, dialog_id: int, access_hash):
    """InputPeer from what the database knows. No API resolve is needed:
    pull.py has already brought the access_hash, and a resolve is one more
    trace."""
    from telethon.tl.types import (
        InputPeerChannel, InputPeerChat, InputPeerSelf, InputPeerUser,
    )
    if kind == "saved_messages":
        return InputPeerSelf()
    if kind == "chat":
        # Ordinary groups have no access_hash: they are addressed by id.
        return InputPeerChat(dialog_id)
    if access_hash is None:
        return None
    if kind == "user":
        return InputPeerUser(dialog_id, access_hash)
    return InputPeerChannel(dialog_id, access_hash)


def mark(db: sqlite3.Connection, dialog_id: int, msgs: list[int],
         batch_id: str, state: str, error: str | None = None) -> None:
    db.executemany(
        "INSERT OR REPLACE INTO deletions "
        "(dialog_id, msg_id, batch_id, state, attempted_at, error) "
        "VALUES (?,?,?,?,?,?)",
        [(dialog_id, m, batch_id, state, int(time.time()), error) for m in msgs],
    )
    db.commit()


def plan_report(by_dialog: dict, db: sqlite3.Connection) -> list[tuple]:
    """Lay the plan out into chunks, and show it to a human on the way."""
    work = []
    for dialog_id, d in by_dialog.items():
        chunks = ranges(db, dialog_id, d["msgs"])
        if not chunks:
            continue
        peer_ok = d["kind"] == "chat" or d["kind"] == "saved_messages" \
            or d["access_hash"] is not None
        work.append((dialog_id, d, chunks, peer_ok))
    # Oldest first. Not cosmetic: if the run has to be cut short, it is cut
    # short on the fresh material — and the fresh is what gets looked at first.
    work.sort(key=lambda w: w[1]["oldest"])
    return work


async def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--yes", action="store_true",
                    help="actually delete; without it, a dry run that stays offline")
    ap.add_argument("--dialog", type=int, action="append",
                    help="limit to a dialog (repeatable) — for a trial run")
    ap.add_argument("--limit", type=int,
                    help="no more than N chunks in one run")
    args = ap.parse_args()

    db = sqlite3.connect(DB)

    pending = db.execute(
        "SELECT COUNT(*) FROM deletions WHERE state = 'pending'"
    ).fetchone()[0]
    if pending:
        print(f"WARNING: {pending} messages are still `pending` — the last run "
              f"broke off on a call and it is unknown whether the deletion went "
              f"through. Sort them out by hand; they are skipped here.\n")

    by_dialog = load(db, args.dialog)
    if not by_dialog:
        approved = db.execute("SELECT COUNT(*) FROM plan WHERE approved = 1").fetchone()[0]
        total = db.execute("SELECT COUNT(*) FROM plan").fetchone()[0]
        sys.exit(f"nothing to delete: {total} messages in the plan, {approved} "
                 f"approved. Approve them with `glisser approve`.")

    work = plan_report(by_dialog, db)
    if args.limit:
        # Trimmed by chunks, not by dialogs: the limit is for a trial run.
        trimmed, left = [], args.limit
        for dialog_id, d, chunks, ok in work:
            if left <= 0:
                break
            trimmed.append((dialog_id, d, chunks[:left], ok))
            left -= len(chunks[:left])
        work = trimmed

    total_msgs = sum(len(c) for _, _, chunks, _ in work for c in chunks)
    total_chunks = sum(len(chunks) for _, _, chunks, _ in work)
    blocked = [w for w in work if not w[3]]

    print(f"dialogs  : {len(work)}")
    print(f"chunks   : {total_chunks}")
    print(f"messages : {total_msgs}")
    mid = lambda s: sum(s) / 2
    est = (total_chunks * mid(BETWEEN_RANGES)
           + len(work) * mid(BETWEEN_DIALOGS)
           + total_msgs * mid(READ_PER_MSG)
           + total_chunks / mid(BREAK_EVERY) * mid(BREAK_FOR))
    end = time.strftime("%H:%M", time.localtime(time.time() + est))
    print(f"will take: ~{est / 3600:.1f} h at a human pace, until ~{end}\n")

    for dialog_id, d, chunks, ok in work:
        flag = "" if ok else "  <- NO access_hash, will skip"
        sizes = ", ".join(str(len(c)) for c in chunks[:8])
        more = " …" if len(chunks) > 8 else ""
        print(f"  {d['title'][:40]:42} {len(chunks):3} chunks [{sizes}{more}]{flag}")

    if blocked:
        print(f"\n{len(blocked)} dialogs without access_hash — run pull.py.")

    if not args.yes:
        print("\ndry run: the network was not touched, nothing was deleted. "
              "Add --yes to delete")
        return

    # --- past this point only with --yes: the irreversible starts here ---
    import tdata_compat
    tdata_compat.apply()
    from opentele.td import TDesktop
    from opentele.api import UseCurrentSession
    from telethon.errors import FloodWaitError, PeerFloodError

    if not TDATA.exists():
        sys.exit(f"no {TDATA}")
    tdesk = TDesktop(str(TDATA))
    if not tdesk.isLoaded():
        sys.exit("tdata did not load")
    client = await tdesk.ToTelethon(str(SESSION), UseCurrentSession)
    await client.connect()
    me = await client.get_me()
    print(f"\nsigned in as {me.first_name} (id {me.id}) — starting\n")

    batch_id = time.strftime("%Y%m%dT%H%M%S")
    done = failed = 0
    seen_chunks = 0
    next_break = random.randint(*BREAK_EVERY)
    started = time.time()

    for n, (dialog_id, d, chunks, ok) in enumerate(work):
        if not ok:
            continue
        if n:
            await asyncio.sleep(pause(BETWEEN_DIALOGS))
        peer = peer_of(d["kind"], dialog_id, d["access_hash"])
        print(f"[{d['title'][:40]}]", flush=True)

        for k, chunk in enumerate(chunks):
            if k:
                await asyncio.sleep(pause(BETWEEN_RANGES))

            seen_chunks += 1
            if seen_chunks >= next_break:
                brk = pause(BREAK_FOR)
                left = total_chunks - seen_chunks
                el = time.time() - started
                eta = el / seen_chunks * left / 3600 if seen_chunks else 0
                print(f"  -- break {brk / 60:.0f} min; done {seen_chunks}"
                      f"/{total_chunks} chunks, {done} messages, "
                      f"~{eta:.1f} h left", flush=True)
                await asyncio.sleep(brk)
                next_break = seen_chunks + random.randint(*BREAK_EVERY)

            # Re-read the chunk before deleting: that is what a live person's
            # client does while scrolling and selecting. It also shows how
            # many of them are still there.
            try:
                alive = await client.get_messages(peer, ids=chunk)
                have = sum(1 for m in alive if m is not None)
            except Exception as e:
                print(f"  could not read the chunk: {e}", flush=True)
                have = len(chunk)
            await asyncio.sleep(len(chunk) * pause(READ_PER_MSG))

            # The intent is recorded BEFORE the call.
            mark(db, dialog_id, chunk, batch_id, "pending")
            try:
                try:
                    # revoke=False: delete on our side only. This phone is
                    # the one being inspected, and deleting for the other
                    # person leaves them a «message deleted» and a question.
                    await client.delete_messages(peer, chunk, revoke=False)
                except FloodWaitError as e:
                    # Wait exactly as long as we were told, plus a little:
                    # working around the limit is the behaviour they look for.
                    wait = e.seconds + random.uniform(5, 20)
                    print(f"  FLOOD_WAIT {e.seconds} s — waiting {wait:.0f} s", flush=True)
                    await asyncio.sleep(wait)
                    await client.delete_messages(peer, chunk, revoke=False)
                mark(db, dialog_id, chunk, batch_id, "done")
                done += len(chunk)
                print(f"  chunk {k + 1}/{len(chunks)}: {len(chunk)} messages "
                      f"({have} of them still there)", flush=True)
            except PeerFloodError:
                # Not «wait a while» but «you behave like a bot». The only
                # right answer is to stop at once: the next step after this
                # warning is a restriction on the account. What is done is
                # done; the rest can be finished tomorrow, the run resumes.
                mark(db, dialog_id, chunk, batch_id, "error", "PEER_FLOOD")
                print("\nPEER_FLOOD — Telegram reads this as automated behaviour. "
                      "Stopping. Resume no earlier than a day from now, with the "
                      "same command.", flush=True)
                break
            except Exception as e:
                mark(db, dialog_id, chunk, batch_id, "error", str(e)[:200])
                failed += len(chunk)
                print(f"  chunk {k + 1}/{len(chunks)}: ERROR {e}", flush=True)
        else:
            continue
        break  # PEER_FLOOD inside — leave the loop over dialogs too

    print(f"\ndeleted : {done}")
    print(f"errors  : {failed}")
    print(f"time    : {(time.time() - started) / 3600:.1f} h")
    db.close()
    await client.disconnect()


if __name__ == "__main__":
    asyncio.run(main())
