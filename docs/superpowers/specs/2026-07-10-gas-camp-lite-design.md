# Gas Camp Lite — Design Spec

| Field | Value |
|---|---|
| Date | 2026-07-10 |
| Status | Design approved in brainstorming (operator, 2026-07-10); awaiting spec review |
| Product | `gas-camp-lite` — a standalone, public Claude Code plugin |
| Origin | Extraction of the orchestration process proven in this repo (v1 phases 1–15 and the dispatch-lifecycle run of 2026-07-09/10: six PRs through plan gate → TDD → dual verification → fix-all → clean-loop merge, including crash and stranding recovery) |

## 1. What it is

A lightweight, canned introduction to multi-agent orchestration for Claude
Code users: the lead/teammate lifecycle this repo runs its phases with,
packaged as a plugin. No daemon, no ledger, no Rust — Claude Code
primitives only. The pitch: "learn agent orchestration from a plugin, not
a platform."

**Requirements (documented, soft-checked):** the superpowers plugin
(worker kickoffs reference its skills: writing-plans, test-driven-
development, verification-before-completion, dispatching-parallel-agents,
using-git-worktrees) and the project forge's CLI (`gh`, `glab`, or `fj`).

## 2. Settled decisions (operator, 2026-07-10)

1. **Distribution:** public marketplace plugin in its own public repo
   `gas-camp-lite`.
2. **v1 scope:** the full lifecycle (lead skill, subagent-hygiene, init
   generator, Stop-hook async guard, SessionStart soft-check). No demo
   project in v1.
3. **Opinionation:** one canonical process, no knobs. The init interview
   parameterizes project FACTS only (gates command, conventions, forge,
   reviewer model, merge policy), never process steps. The escape hatch is
   the generated, user-owned guide — people who outgrow defaults edit
   their guide, never the plugin.
4. **Name:** `gas-camp-lite`. (Rejected: "Base Camp" — collides with
   37signals' Basecamp; descriptive names lose the family tie.)
5. **Architecture:** Approach A, generator-centric — thin plugin (fixed
   behavior), thick generated artifact (per-project data). Skills carry
   behavior; the generated guide carries the project's verbatim kickoff
   blocks and facts. This is the exact skill-vs-guide division that held
   in production here.
6. **Multi-forge:** GitHub, GitLab, and Forgejo/Codeberg supported via a
   forge-verbs table in the generated guide. GitHub is tier-1 (end-to-end
   exercised); GitLab/Forgejo are tier-2 in v1 (generation tested, verbs
   verified against real CLIs, not orchestration-exercised — stated
   plainly in the README).

## 3. Components

### 3.1 Skills (fixed behavior)

- **`orchestrating-phases`** — the lead's contract, generalized from this
  repo's `phase-orchestration`: dispatcher-not-worker role rules (never
  edits deliverables, never reads diffs, never debugs); ready check
  (dependencies MERGED, not green-but-unmerged); plan gate (teammate's
  first deliverable is an execution-ready plan doc, reviewed by a
  dispatched plan reviewer before any execution; binary APPROVE/REJECT;
  reject loops with fresh passes); kickoff composition (PREAMBLE + phase
  block from the project guide, VERBATIM — a wrong block is fixed via PR
  to the guide, never paraphrased); execution tracking; the lead
  verification checklist (run the checkable steps yourself — CI via the
  forge CLI, rebase state, plan doc committed with approval note);
  dual verification on every phase PR (a code review AND an independent
  assessment that exercises the change for real); fix-all (every finding
  from every pass relays to the owning teammate immediately; revise →
  fresh-review until a pass returns clean); merge policy read from the
  guide (operator-merges-all, or delegated-merge-on-clean-loop); the
  post-merge rebase protocol; the fresh-lead recovery protocol; and the
  red-flags table. Model references are never hardcoded — the skill says
  "your strongest available model"; the concrete choice lives in the
  guide.
- **`subagent-hygiene`** — ported from this repo's current (2026-07-10,
  post-#56) version, near-verbatim: callbacks don't wake stopped
  sessions; armed watchers are not wake guarantees; stay-in-turn-and-poll;
  the report ships in the verifying turn and "ships" means transmitted;
  file handoffs; explicit addressing; permission envelopes; parent-side
  stall recognition. The incident log is reframed as dated case studies
  (this repo's sessions are the evidence base — that specificity is the
  persuasive core and is kept, with repo-identifying paths generalized).

### 3.2 Commands (machinery)

- **`/gas-camp-lite:init`** — interviews the user (one question at a
  time) and generates the per-project orchestration guide. Questions, in
  order: forge (GitHub/GitLab/Forgejo — defaulted from the remote URL);
  gates command(s); branch naming convention + default-branch rule;
  plan-doc directory; reviewer model (default "strongest available");
  merge policy; worktree convention (harness-native isolation vs. a
  `.worktrees/` dir). House rules are NOT asked — the PREAMBLE points
  teammates at the project's CLAUDE.md, which owns them.
- **`/gas-camp-lite:status`** — read-only board recap for a returning
  lead: the guide's dependency map vs. `<forge> list-merged`/open, plus
  the harness task list.
- **`/gas-camp-lite:recover`** — walks the fresh-lead recovery protocol
  (PR states → in-flight branches/worktrees → reattach or respawn from
  guide kickoffs; branches, plan docs, and PRs carry all real state).

### 3.3 Hooks (enforcement — the genuinely new build)

- **Stop guard** — fires on `Stop` and `SubagentStop`. If harness-tracked
  background tasks are live, BLOCK the stop with a message enumerating
  them plus the three sanctioned moves (foreground-watch / hand the wait
  to a party whose completion messages you / stay in-turn and poll).
  Tri-state config in the guide: `block` (default) / `warn` / `off` —
  this is the ONE deliberate exception to decision 3's no-knobs rule,
  permitted because it configures an enforcement mechanism, not a
  lifecycle step, and because hard-blocking must be relaxable without
  forking the plugin. NO per-stop override token — an inline escape hatch
  is a rationalization surface; loosening is a deliberate guide edit. Documented limitation:
  the guard sees only harness-tracked work — detached processes and
  external CI are invisible to it, which is why the skill (judgment) and
  the hook (mechanism) ship together.
- **SessionStart check** — warn-only, two checks: superpowers plugin
  present (plugin-cache lookup; message names why it's needed), and the
  guide's plugin-version stamp vs. the installed plugin version (warn on
  major mismatch; suggest re-running `/init` and diffing).

### 3.4 Templates (consumed by init, not user-facing)

Guide skeleton containing, in order: authority line (operator > project
docs > this guide); dependency-map table scaffold with one worked example
row; the PREAMBLE as a fenced block with interview answers baked in; a
phase-block scaffold plus one filled example; the shared-files/rebase
protocol; the lead verification checklist with the project's gates
command inlined; the forge-verbs table (§4); the escalation list; the
recovery protocol; the Stop-guard config line; and the plugin-version
stamp. Three forge template variants feed the forge-specific rows.

## 4. The forge-verbs table

The five operations the lifecycle needs, with the project's concrete
commands baked into the guide at init time:

| Operation | Used by |
|---|---|
| list-merged (ready check) | lead ready check, /status, /recover |
| view state (open/merged/mergeable/head) | lead verification checklist |
| watch-checks-to-terminal | teammates' CI watch; lead re-verification |
| merge | the merge step (per merge policy) |
| terminology token (PR / MR) | all generated prose |

Where a CLI lacks a true watch-to-terminal mode, that forge's recipe is
the sanctioned in-turn poll loop from subagent-hygiene — no new concept.

**Implementation rule (binding):** the exact `glab` and `fj` subcommands
are verified against the real CLIs (or their authoritative docs at a
pinned version) during implementation — never written from memory. The
plan must carry the verified command table with provenance.

## 5. Data flow in use

Lead session → `orchestrating-phases` fires → "read your project's
orchestration guide now" → kickoffs composed verbatim from the guide's
blocks → teammates receive kickoffs that reference superpowers skills
(writing-plans for the plan gate, TDD, verification-before-completion)
and subagent-hygiene → plan gate → execution → lead verification
checklist → dual verification → fix-all loop → merge per policy →
post-merge rebase protocol → next window.

## 6. Testing

- **Skills** — writing-skills TDD in a CLEAN project with zero gascamp
  context. Pressure scenarios: for `orchestrating-phases`, a lead tempted
  to edit code itself, skip the plan gate, and merge on a dirty loop; for
  `subagent-hygiene`, the delayed-token retrieval scenario (an untracked
  process writes a secret token after a delay; stopping to wait = strand;
  pass = unprompted transmitted report with the exact token) — already
  proven as this repo's standard pressure test. API-costing, therefore a
  documented RELEASE CHECKLIST, not CI.
- **Hooks** — fixture-driven, CI-able: recorded Stop/SessionStart stdin
  payloads → assert block/warn/pass decisions. bash/jq only.
- **Init** — golden-output per forge (three goldens): scripted interview
  answers → generated guide compared byte-for-byte minus the version
  stamp line.
- **CI** — GitHub Actions: hook fixtures + init goldens + markdown lint.

## 7. Packaging, versioning, relationships

- **README:** the pitch; quickstart (install → `/init` → first
  orchestrated phase); a why-each-piece-exists section mapping every
  mechanism to the failure it prevents, citing the dated case studies
  (extraction from production, not theory — the differentiator); the
  requirements; the tier-1/tier-2 forge support statement; the Stop-guard
  limitation.
- **License:** MIT (operator may override at spec review).
- **Versioning:** semver. Guide carries the plugin version; SessionStart
  warns on major mismatch. Discipline: any PREAMBLE/block-template change
  bumps at least minor with a release note telling users to re-run
  `/init` and diff.
- **Relationship to this repo:** extraction, not relocation — gascamp
  keeps its own project-tuned skills untouched. The plugin README carries
  one forward-looking line about a daemon-grade sibling without naming
  the private repo; the full ladder story (Gas City → Gas Camp →
  gas-camp-lite as on-ramp) waits until Gas Camp is public.

## 8. Out of scope for v1

- Demo/tutorial project (deferred; revisit for v2).
- Effort profiles or any process knobs.
- Daemon/ledger machinery of any kind.
- Forges beyond GitHub/GitLab/Forgejo; forge CLIs beyond gh/glab/fj.
- End-to-end orchestration exercising on GitLab/Forgejo (tier-2 in v1).

## 9. Risks and mitigations

- **Guide drift vs. plugin version** → version stamp + SessionStart warn
  + release-note discipline (§7).
- **Skill wording that binds here but not in clean projects** → the
  clean-project pressure tests are the release gate; the writing-skills
  Iron Law applies to every ported skill (no edit ships untested).
- **Forge-verb rot** (glab/fj CLIs evolve) → verbs live in the generated
  guide (user-fixable without a plugin release) + verified-at-pinned-
  version provenance in the repo.
- **Stop-guard false blocks** (legitimate stops with a long-running but
  irrelevant tracked task) → the message names the tasks so the model can
  finish or hand them off; tri-state config for projects that want
  `warn`; no inline override by design.
