# Control-wire fixtures — provenance

Every fixture in this directory is LABELLED. The label says what the bytes
ARE, and — just as importantly — what is NOT claimed about them. The `claude`
control protocol is undocumented; the only honest way to pin it is to say
exactly where each shape came from and how far the evidence reaches.

## The labels

| label | means |
|---|---|
| `recorded-from-CLI-<v>` | these exact bytes were OBSERVED on the wire from the real CLI |
| `derived-from-CLI-<v>`  | the KEYS were extracted from the shipped CLI bundle; the VALUES are illustrative |
| `camp-authored`         | camp invented these bytes. Any acceptance claim is stated separately and is backed by a gate |

## The method, and its limits

The CLI is a minified single-file bundle. `sdk.mjs` is not vendored, so the
shapes were recovered from the installed binary with `strings`:

```bash
CLI=$(readlink -f "$(command -v claude)")     # must equal ci/claude-compat/CLAUDE_VERSION
strings -a "$CLI" | grep -o 'subtype:"can_use_tool".\{0,400\}'
strings -a "$CLI" | grep -o 'subtype:"request_user_dialog".\{0,300\}'
strings -a "$CLI" | grep -o 'type:"control_response",response:{subtype:"error".\{0,60\}'
strings -a "$CLI" | grep -o 'type==="control_request"&&.\{0,40\}'
strings -a "$CLI" | grep -o '.\{0,150\}updatedInput?: object}.\{0,60\}'
strings -a "$CLI" | grep -o 'sendResponse(r,n).\{0,120\}'
```

Run 2026-07-13 against claude 2.1.207/2.1.208 (the two agree verbatim on
every shape below; only minified variable names differ).

**THE METHOD CANNOT PROVE KEY-COMPLETENESS.** A fixed-width window on a
minified bundle shows what is at the construction site it matched — never
that no other site adds more keys. In fact a SECOND `can_use_tool`
construction site adds `decision_reason`, `decision_reason_type`,
`classifier_approvable` and `agent_id`. **camp's parse is therefore
deliberately tolerant** (`Envelope` is NOT `deny_unknown_fields`), and camp
reads only the keys it actually needs. A fixture is a pin, not a schema.

## The files

| file | label | notes |
|---|---|---|
| `interrupt_request.json` | **camp-authored** | **ACCEPTED** by CLI 2.1.207/208 — Task 10's $0 gate sends exactly these bytes to the real CLI and asserts the ack. The claim is **ACCEPTANCE, not recording**. |
| `control_response_success.json` | **recorded-from-CLI-2.1.207** | observed on the wire from a live $0 run (a pre-turn interrupt; `still_queued` is empty). |
| `control_response_error.json` | **derived-from-CLI-2.1.207** | the envelope is verbatim from the bundle (`response:{subtype:"error",request_id:…,error:…}`); the `error` STRING is illustrative. The `error` KEY is verified. |
| `can_use_tool_request.json` | **derived-from-CLI-2.1.207** | KEYS from the bundle (400-char window); VALUES illustrative. The conditional `permission_suggestions` / `blocked_path` spreads are OMITTED, and a second construction site adds four more keys. **Completeness is NOT claimed.** |
| `request_user_dialog_request.json` | **KEYS derived-from-CLI-2.1.207; `dialog_kind`'s VALUE is camp-invented** | `dialog_kind`'s value set is a minified variable and was NOT recovered. **camp must never key on it** — it refuses every dialog and reads only `request_id`. |
| `dialog_refusal_response.json` | **camp-authored**, shape mirrored from the CLI's own error-response construction | **UNVALIDATED against the real CLI.** camp sends it only under `--permission-prompt-tool stdio`, which is phase 3, so no $0 gate here can exercise it. **PHASE-3 OBLIGATION:** if the shape is wrong the CLI ignores it and the worker hangs forever — the outcome §9 exists to prevent. |
| `permission_allow_response.json` | **derived-from-CLI-2.1.207** | **For phase 3 (cp-3). cp-1 does not send it.** The CLI's own validator names the contract: `Expected {behavior: 'allow', updatedInput?: object} or {behavior: 'deny', message: string}.` Pinned here so cp-3 inherits recovered bytes instead of a guess. |
| `permission_deny_response.json` | **derived-from-CLI-2.1.207** | as above. |
| `user_turn.json` | **camp-authored** | the bytes `spawn::user_message` ACTUALLY produces (`serde_json::json!` sorts keys — serde_json 1.0.150 has no `preserve_order`). **ACCEPTED by the CLI**: this exact envelope is probe P2 and has shipped since Phase 8. The key order is ugly and it is CORRECT — do not "tidy" `user_message` into a struct to make it prettier; that would change the bytes every production dispatch sends, and this pin is what catches such a change. |
| `stream_assistant.json` | **camp-authored** | a representative NON-control stream line. camp never interprets it (D3: the transparent stream surface) — it exists so the passthrough test can assert the bytes are handed on unchanged. |

## What is pinned where

- `daemon/control.rs`'s unit tests pin every shape camp **sends** (byte-equal)
  and every shape camp **parses**.
- `tests/claude_compat.rs`'s `#[ignore]`d $0 gate sends
  `interrupt_request.json` to the **real** CLI. That is what makes
  "camp-authored" and "the CLI accepts it" both true, and it is the only
  claim of acceptance this directory makes.
