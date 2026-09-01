# 6lisser

*Granular cleanup of your Telegram history across 6 axes.*

An entertaining way to spend an evening — or to trim a fifteen-year political
sentence down to a three-minute pleasant chat at the border. There's something
here for everyone. The internet was and always will be a place of free thought
and intent; there are just a few more reefs beneath the surface now — the trick
is to glide.

## Principle

Cleanup is soft: keep as much as possible, cut only what's sharp. An account
scrubbed to sterility is worse than a compromising one — the seams must not show.
After a pass the conversation reads bland, ordinary, whole.

The naturalness of correspondence is the best ally.

**Two levels.**

*The detector*. A cheap pass over dictionaries decides not *what* to cut but
*where to look*: it flags candidates and stays silent on the rest. The
dictionaries live in the source, one array per rule — extending one is an edit
and a rebuild. The pass costs almost nothing: it casts a wide net, and the
second level removes the excess.

*The model*. Reads candidates window by window and picks the segment for the
knife: not a lone line but a connected stretch that leaves no dangling argument
behind. The older the correspondence, the softer the threshold. It also marks
what to keep on purpose, not only what to cut.

The last word belongs to a human. The plan passes a manual gate. Deletion runs
at a human pace and on one's own side only — nothing flickers for the other
person. Everything is local, not a stray touch to the network.

## Pipeline

```bash
glisser export                  # load a Telegram Desktop export — no API is touched
glisser scan                    # dictionary pass → candidates
glisser windows                 # estimate how many LLM calls this will cost
glisser triage --jobs 16        # the model decides what to cut
glisser plan --model <model>    # assemble the deletion plan from verdicts
glisser approve --axis politics # manual gate: only what's approved gets deleted

python purge/pull.py            # read-only: brings the access_hash over MTProto
python purge/purge.py           # dry run: shows what it would delete, no network
python purge/purge.py --yes     # deletion, at a human pace
```

Ingestion comes from an offline Telegram Desktop export and makes no API calls
at all. The network only wakes for the last two steps.

## Build

```bash
cargo build --release
```

Thresholds, the whitelist, the language of the policy and the optional owner
profile live in `src/config.rs`: edit it and rebuild. The dictionaries are
`src/dict.rs`, the policy itself is `src/prompt.rs`. Nothing is read from disk
at run time; everything a run writes stays in `state/`.

The owner profile is optional in every field. Left empty, the policy simply
runs in its general, more conservative mode.

---

With all my heart,

38b23001f6b9f09f1d4034569a5bc8aac9a53b246bda4021c7c862858308d18b
