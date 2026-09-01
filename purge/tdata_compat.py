"""opentele 1.15.1 against tdata from Telegram Desktop 12.x.

There is exactly one problem. TD 12.x writes new record types into the `map`
file (`lskCustomEmojiKeys=0x17`, `lskSearchSuggestions=0x18`,
`lskWebviewTokens=0x19`) that opentele does not know: on `0x17` the map parse
dies with `TDataReadMapDataFailed: Unknown key type in encrypted map: 23` and
no account loads. The crypto is intact — the map decrypts fine, only the
parser trips.

Why skipping the map is the safe fix rather than teaching it the new types:
the auth data (authKey, dcId, userId) does not live in the map but in a
separate mtp file read by `readMtpData()`. The map only holds keys of
auxiliary files — drafts, stickers, emoji — which we do not need. `localKey`
for mtp is set in `StorageAccount.start()` BEFORE the map is read, so a map
parse failure blocks nothing downstream.

That also survives the next TD versions with yet more record types: there is
no hardcoded table of types to maintain.

The patch writes nothing into tdata, installs nothing and touches no network.
"""
import sys

from opentele.exception import OpenTeleException
from opentele.td.account import StorageAccount
from PyQt5.QtCore import QByteArray

_applied = False


def apply() -> None:
    """Patch opentele. Idempotent; call once before TDesktop(...)."""
    global _applied
    if _applied:
        return

    def readMapWith(self, localKey, legacyPasscode=QByteArray()):
        # Unlike the original (return False on any map error) this does not
        # stop: the map is not needed for auth, and its TD 12.x format is newer.
        try:
            self.mapData.read(localKey, legacyPasscode)
        except OpenTeleException as e:
            print(f"[tdata_compat] map not parsed, skipping: {e}", file=sys.stderr)
        self.readMtpData()

    StorageAccount.readMapWith = readMapWith
    _applied = True
