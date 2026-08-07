# Design rationale

## Cost is reconstructed, never reported

Nothing upstream tells us what a block cost: the statusline re-derives it from transcript token
counts times published per-token prices. Every rule below exists because a reconstruction can be
silently wrong in ways a billed figure cannot, and the failure always looks like a plausible
number rather than an error.

## Cache writes are priced by TTL, and the unqualified field is the short one

LiteLLM's plain cache-creation field is the short-TTL write rate; the long-TTL rate lives in a
separate, less-known field. Claude Code emits both TTLs depending on the surface, so pricing every
write from the unqualified field silently under-charges the long-TTL ones, and cache creation is a
large share of an agentic session's spend.

Transcripts carry the per-TTL breakdown alongside the flat total. Price from the breakdown; fall
back to the flat total at the short-TTL rate only when the breakdown is absent, since that is the
only case where the TTL is genuinely unknown.

The long-TTL field is only trustworthy on current models — on retired ones its published value
bears no consistent relation to that model's base input price. Prefer the field, but sanity-check
it against base input rather than trusting it blindly.

## The long-context tier is per-request, not per-category

Where a model has an above-threshold price tier, the tier is chosen once from the request's total
prompt size and then applies to every token category in that request, output included. Charging
each category against its own threshold — the first N of each at base, the remainder at premium —
under-reports, because a request whose prompt clears the threshold on cache reads alone still
prices its modest input and output at base.

Newer models include their full window at standard pricing and have no tier, which LiteLLM
represents by omitting the above-threshold fields. Absent fields must therefore mean "same as
base", so tier selection becomes a no-op rather than a special case.

## The context denominator is the managed window, not the model window

Claude Code's statusline payload reports the raw model window and a percentage derived from it.
That is not the limit the user hits: auto-compact triggers against a smaller managed window,
reduced further by an output reserve and a compaction headroom, and the managed window is never
exposed to us. Showing the payload's percentage means showing a number that stays comfortable
while the session is about to compact.

So the denominator is reconstructed: model window, narrowed by the auto-compact policy when
auto-compact is on, then by the reserves. Two consequences that are easy to get backwards —
disabling auto-compact *raises* the usable window rather than lowering it, and it raises it by
different amounts on different models, because only some models' windows are policy-narrowed in
the first place. A single "compacted vs full" pair of constants cannot express that.

When a payload percentage is displayed, the token count shown beside it must be the quantity that
percentage was computed from. Pairing a percentage from one total with a count from another
renders one element whose two halves disagree.

## Model identity is a string with a mode suffix

A model id can carry a bracketed suffix marking the long-context mode. The suffix is the only
signal that an otherwise-standard model is running with the large window, so parsing must consult
it before falling back to matching the bare id — stripping it first discards the answer.

## Transcript discovery: the whole subtree, and per-file staleness

Sub-agent transcripts live below the session, not beside it, and their tokens bill to the same
block as the orchestrator's. A scan that only reads the session level silently omits them, which
on agent-heavy work is most of the spend. Discovery walks the project subtree.

The staleness cutoff applies to files, never to directories. Appending to a transcript does not
touch its directory's mtime, so pruning by directory drops exactly the long-running session the
statusline exists to report on.

## Block boundaries chain, so the scan window cannot truncate them

A block anchors at the first entry after the previous block expired, which makes every boundary a
function of all earlier activity. Truncating the entry stream at a lookback horizon does not just
drop old cost, it re-anchors the whole chain and moves the active block's start — corrupting both
the cost and the reset countdown for exactly the continuous-use sessions the horizon was meant to
keep cheap.

Boundaries therefore come from the authoritative reset time whenever one is available. The derived
chain is the fallback and is knowingly approximate: no horizon makes it exact on a continuously
used account, and the ones that come close cost several times the render budget.

## Transcripts are append-only, and everything fast depends on it

Reading only a suffix, and resuming a parse from where the last one stopped, are both
valid only while transcripts are extended rather than rewritten. A rewrite that leaves
the file no shorter is undetectable and yields a wrong cost with no error, so anything
editing a transcript in place must invalidate the parse cache explicitly.

That cache holds token counts and dedup keys rather than costs: prices move
independently of transcripts, and deduplication spans files so it cannot be re-derived
from any one of them.

## A locked file is written in place; only unlocked ones are published by rename

Because `flock` binds to an inode, renaming over a locked path strands every waiter on the
unlinked file: they read pre-rename content, merge into it, and publish their result over the
winner's. The symptom is a usage percentage going backwards, not an error.

So the two publishing strategies may not be mixed per file. Any file readers take a shared lock on
is written in place under the exclusive lock — this binds every such file, including the API
response cache, not only the stores that merge. Rename-based publishing is reserved for files read
without a lock, and its temp name must be unique per writer, since a temp path derived from the
destination alone is shared by all concurrent writers who then interleave into it.

## Concurrency is the normal case

Every terminal running Claude Code renders this statusline, so several processes hit the same
caches and the same upstream endpoints at once. A cold cache must not turn that into a fan-out of
simultaneous fetches: the authenticated usage endpoint is the binding case, where racing renders
earn the very rate-limit response the backoff logic exists to absorb, so its cold path is
serialized behind the cache lock.

Read paths degrade to a cache miss rather than an error. A failed render produces no output at
all, so any recoverable condition — a transcript deleted mid-session, an unreadable config, a
corrupt cache — falls back to a usable value instead of propagating.

Diagnostics for those fallbacks go to stderr only when it is a terminal, because Claude Code
neither shows nor discards statusline stderr predictably. That makes the interactive run the place
a misconfiguration becomes visible, and it is why silently returning a default — rather than
falling back loudly — leaves a user with no way to discover the problem at all.
