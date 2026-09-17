# Forum translation node

The Forum mode reads committed translation coverage from the local core. It
does not create production translation work from Dora's transient ASR text.
The core persists the original transcript independently, before translation.

`FORUM_RUNTIME_CONFIG` contains the authenticated Unix endpoint and session
scope supplied by the host. The node derives the `translator` producer journal
with `RuntimeConfig::for_producer`. Dora still registers and owns the model
process. The `ready` RPC is sent only after the actual MLX load/warmup receipt
and replayable outbox events have obtained core receipts. It reports the durable
prior run IDs and whether the outbox is empty; retained fenced results never
become a false replay acknowledgement.
Missing or malformed runtime configuration fails explicitly. Historical F01
diagnostics can opt into the old, non-durable text pipeline with
`FORUM_TRANSLATOR_ALLOW_LEGACY=1`; that is not the production Forum mode.

The translation lifecycle is:

1. Replay pending producer events with their original message/run/sequence IDs.
2. Read `translation_requests` for unresolved requested/failed attempts, then
   `poll_translation` for unclaimed source coverage. The host filters unresolved
   states before applying its query limit. Recovery walks `items`/`next_after`
   pages with a stable first-created sequence and translation ID; an exhausted
   first page cannot hide later work. Only one page is read per scheduling poll.
3. Build a request from exact source revisions and UTF-8 spans. Write
   `translation.requested` to the producer outbox and obtain its durable core
   receipt before handing text to the model.
4. Persist `translation.final` or `translation.failed` in the producer outbox.
   Remove it only when core acknowledges that exact event. A core commit followed
   by a lost receipt is replayed rather than regenerated.

Only one request generates at a time. A recovered request keeps the same
translation ID/revision and increments attempt, with at most three automatic
attempts across worker restarts. A user-requested recovery may set
`FORUM_RETRY_EXHAUSTED=1` together with a nonnil
`FORUM_TRANSLATOR_RECOVERY_ID`. The host must keep that action UUID unchanged
across child restarts and clear both variables for ordinary new sessions. A
private, synced `.budget` file freezes each translation’s ceiling at its initial
attempt plus three for that action. A restart or ordinary poll never renews the
allowance; only another explicit user recovery action can grant another budget.
Duplicate local completions are ignored; core
checks source revision, attempt and direction epoch again when committing. A
`LATE_RESULT` rejection remains in the local outbox for inspection and is never
displayed as an accepted final. The core currently needs a separate explicit
discard receipt before such a rejected file could be removed safely.

`AttributedBuffer` keeps source provenance for every retained piece. Inserted
spaces/newlines have no source identity. Requests use `identity-v1`,
`join-space-v1` or `join-newline-v1`, so core can reconstruct the exact model
input. Splitting uses UTF-8 and grapheme boundaries, preserving combining marks
and emoji sequences. Production preserves original punctuation and outer source
whitespace; the explicit trim helper adjusts offsets rather than retaining
incorrect citations. A short, unpunctuated tail has no ten-character threshold:
its core coverage remains pending until requested, or recoverable after Stop.

Different targets, epochs and passthrough routes never share a request. Model
language detection remains separate from the configured source hint. Unknown or
mixed language does not silently become English. Mixed Chinese/English can have
both target coverages; empty targets produce no translation work. Same-language
text can use an explicit `passthrough` result without submitting a model request.
Script checks only prevent unsafe passthrough; they are not audio detection.

Generation checks a shared cancellation flag and a total 45-second budget at
token boundaries. Exhausting the 256-token budget without EOS produces a failure
instead of publishing a truncated final. Stop cancels the in-flight attempt,
persists its failed outcome, and leaves all unclaimed coverage in core. An
unresponsive model kernel is ultimately contained by the owned node process;
these token checks do not claim to preempt a Metal call.

The node computes `sha256:...` with `forum_runtime::model_fingerprint` from the
actual local configuration, tokenizer and weights before loading the backend.
That same content fingerprint goes into ready, requested and final records.
`FORUM_TRANSLATOR_MODEL_MANIFEST_ID`, if nonempty, is only an expected value:
a mismatch stops initialization and cannot replace the computed identity.
Existing requests with a different manifest are never silently relabeled.
No model download happens in this node.

From the repository root, run the model-free mapping, routing, cancellation and
real Unix transport/outbox recovery tests:

```sh
CARGO_TARGET_DIR=/tmp/aivf-cargo-target cargo +stable test --offline --locked \
  --manifest-path desktop/Cargo.toml -p dora-qwen35-translator --bin dora-qwen35-translator
```

The host used by the outbox tests deliberately simulates acknowledgement loss
and fencing; SQLite's transaction/fence implementation is tested separately by
`forum-core`. These tests do not start Dora, open audio devices or load weights.
`examples/forum_translation_probe.rs` remains an explicit local-weight backend
probe. Its optional positive `cancel_after_streams` case field cancels generation
after that many cumulative callbacks; a subsequent ordinary case in the same
process verifies worker recovery. Model measurements and real meeting acceptance are separate from these
deterministic checks.
