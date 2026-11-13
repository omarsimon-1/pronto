#!/usr/bin/env python3
"""Prepares and shows the testbed user `ops` for tests/selftest/testbed-remind.sealwax-test.

  remind_mailbox.py fill SERVER...   drops the Reminders folder, empties every folder of `ops` and
                                     appends three synthetic messages to INBOX: "Review trip" with
                                     Tomas Berg's answer (one conversation) and "Review invoice"
  remind_mailbox.py clean SERVER...  drops the Reminders folder and empties every folder
  remind_mailbox.py show SERVER...   prints each folder's messages as the server holds them

SERVER is dovecot, dovecot-rev1 or stalwart. It touches only `ops`, which the engine's
ops_testbed and remind_testbed suites use too (run one at a time), never starts, stops or
reseeds the testbed, and connects only to 127.0.0.1. The CA is $PRONTO_TESTBED_CA, else
certs/ca.pem beside this file. All content is synthetic.
"""

import datetime
import os
import re
import sys
from pathlib import Path

import seed

USER = "ops"
EXISTS = re.compile(rb"^\* (\d+) EXISTS")
FETCHED = re.compile(rb"^\* \d+ FETCH \(.*FLAGS \(([^)]*)\).*\{(\d+)\}$")


def connect(server: str) -> seed.Imap:
    if server not in seed.SERVERS:
        sys.exit(f"remind_mailbox: unknown server {server}; choose from {', '.join(seed.SERVERS)}")
    ca = os.environ.get("PRONTO_TESTBED_CA")
    if ca:
        # seed.Imap reads certs/ca.pem under seed.HERE.
        seed.HERE = Path(ca).resolve().parent.parent
    return seed.Imap(seed.SERVERS[server][0], USER)


def select(imap: seed.Imap, folder: bytes) -> int:
    tag = imap.tag()
    imap.sock.sendall(tag + b' SELECT "' + folder + b'"\r\n')
    exists = 0
    while True:
        line = imap.reader.readline()
        if not line:
            raise ConnectionError("connection closed")
        match = EXISTS.match(line)
        if match:
            exists = int(match.group(1))
        if line.startswith(tag + b" "):
            if line.split()[1] != b"OK":
                raise RuntimeError(line.decode(errors="replace").strip())
            return exists


def message(n: int, subject: str, sender: str, parent: int | None, now: datetime.datetime):
    """The conversation's messages an hour apart, the newest an hour ago; read."""
    date = now - datetime.timedelta(hours=4 - n)
    headers = [
        f"From: {sender}",
        f"To: <{USER}@example.test>",
        f"Subject: {subject}",
        "Date: " + date.strftime("%a, %d %b %Y %H:%M:%S +0000"),
        f"Message-ID: <{USER}.testbed-remind.{n}@example.test>",
    ]
    if parent is not None:
        reference = f"<{USER}.testbed-remind.{parent}@example.test>"
        headers += [f"In-Reply-To: {reference}", f"References: {reference}"]
    headers += ["MIME-Version: 1.0", "Content-Type: text/plain; charset=utf-8"]
    body = f"Syntetisk melding {n} for testbed-remind.\r\n"
    return ["\\Seen"], date, ("\r\n".join(headers) + "\r\n\r\n" + body).encode()


def empty(imap: seed.Imap):
    """Drops the Reminders folder Pronto made, and empties every other folder."""
    for folder in sorted(imap.folders()):
        if folder.rsplit(b"/", 1)[-1].rsplit(b".", 1)[-1].lower() == b"reminders":
            imap.command(b'UNSUBSCRIBE "' + folder + b'"')
            imap.command(b'DELETE "' + folder + b'"')
    for folder in sorted(imap.folders()):
        try:
            exists = select(imap, folder)
        except RuntimeError:
            continue
        if exists:
            imap.command(b"STORE 1:* +FLAGS.SILENT (\\Deleted)")
            imap.command(b"EXPUNGE")


def fill(server: str):
    imap = connect(server)
    empty(imap)
    now = datetime.datetime.now(datetime.timezone.utc).replace(microsecond=0)
    imap.append_all(
        "INBOX",
        [
            message(1, "Review trip", "Kari Nordmann <kari@example.test>", None, now),
            message(2, "Re: Review trip", "Tomas Berg <tomas@example.test>", 1, now),
            message(3, "Review invoice", "Henrik Aas <henrik@example.test>", None, now),
        ],
    )
    imap.logout()
    print(f"remind_mailbox: {server}: Reminders dropped, every folder emptied, 3 messages in INBOX")


def clean(server: str):
    imap = connect(server)
    empty(imap)
    imap.logout()
    print(f"remind_mailbox: {server}: Reminders dropped, every folder emptied")


def show(server: str):
    imap = connect(server)
    for folder in sorted(imap.folders()):
        try:
            exists = select(imap, folder)
        except RuntimeError:
            continue
        rows = []
        if exists:
            tag = imap.tag()
            imap.sock.sendall(tag + b" FETCH 1:* (FLAGS BODY.PEEK[HEADER.FIELDS (SUBJECT)])\r\n")
            while True:
                line = imap.reader.readline()
                if not line:
                    raise ConnectionError("connection closed")
                match = FETCHED.match(line.rstrip(b"\r\n"))
                if match:
                    header = imap.reader.read(int(match.group(2))).decode(errors="replace")
                    subject = header.replace("Subject:", "").strip()
                    rows.append(f"{subject} [{match.group(1).decode()}]")
                if line.startswith(tag + b" "):
                    break
        print(f"{server} {folder.decode()}: {exists}")
        for row in rows:
            print(f"  {row}")
    imap.logout()


def main():
    actions = {"fill": fill, "clean": clean, "show": show}
    if len(sys.argv) < 3 or sys.argv[1] not in actions:
        sys.exit(__doc__)
    for server in sys.argv[2:]:
        actions[sys.argv[1]](server)


if __name__ == "__main__":
    main()
