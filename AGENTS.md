# AI Agent Instructions

This document provides instructions for AI agents working on this codebase.

# APR (Automated Plan Reviser Pro) — Agent Reference

Iterative spec refinement via GPT Pro Extended Reasoning. Multi-round AI review
with structured outputs for Claude Code integration.

## Commands

# Workflow
apr setup                      # Interactive wizard (first time)
apr run <N>                    # Run revision round N
apr run <N> -i                 # Include implementation doc
apr run <N> -d                 # Dry-run preview
apr show <N>                   # View round output

# Analysis
apr diff <N> [M]               # Compare rounds (N vs M, or N vs N-1)
apr stats                      # Convergence analytics + remaining rounds estimate
apr integrate <N> -c           # Claude Code prompt → clipboard (KEY COMMAND)

# Management
apr status [--hours 24]        # Oracle session status
apr attach <slug>              # Reattach to session
apr list                       # List workflows
apr history                    # Round history
apr backfill                   # Generate metrics from existing rounds
apr update                     # Self-update

## Robot Mode (JSON API)

apr robot status               # {configured, workflows, oracle_available}
apr robot workflows            # [{name, description}, ...]
apr robot init                 # Create .apr/
apr robot validate <N>         # Pre-flight → {valid, errors[], warnings[]}
apr robot run <N>              # Execute → {slug, pid, output_file, status}
apr robot history              # List completed rounds
apr robot help                 # API docs

Response: {ok, code, data, hint?, meta: {v, ts}}
Codes: ok | not_configured | not_found | validation_failed | oracle_error

## Key Paths

.apr/rounds/<workflow>/round_N.md   # ← GPT output (INTEGRATE THIS)
.apr/rounds/<workflow>/metrics/     # Round analytics data
.apr/workflows/<name>.yaml          # Workflow definition
.apr/config.yaml                    # Default workflow

## Agent Workflow

# 1. Validate (saves 30+ min on failures)
apr robot validate 5 -w myspec | jq -e '.data.valid' || exit 1

# 2. Run
result=$(apr robot run 5 -w myspec -i)

# 3. After completion — use integrate command or read file directly
apr integrate 5 -w myspec --copy
# File: .apr/rounds/myspec/round_5.md

# 4. Check convergence to know when to stop
apr stats -w myspec  # Score ≥0.75 = approaching stability

## Reliability Features

- Pre-flight validation before expensive Oracle runs
- Auto-retry with exponential backoff (10s → 30s → 90s)
- Session locking prevents concurrent runs
- Configurable via APR_MAX_RETRIES, APR_INITIAL_BACKOFF

## Options

-w, --workflow NAME   Workflow (default: from config)
-i, --include-impl    Include implementation doc
-d, --dry-run         Preview oracle command
-c, --copy            Copy to clipboard
-o, --output FILE     Output to file
-v, --verbose         Debug output
--wait                Block until completion
--login               Browser login (first time)
--no-preflight        Skip validation
--hours NUM           Status window (default: 72)
--compact             Minified JSON (robot mode)
--json                JSON output for stats command
--detailed            Detailed metrics for stats command

## Dependencies

Required: bash 4+, node 18+, oracle (or npx @steipete/oracle)
Optional: gum (TUI), jq (robot mode), delta (prettier diffs)

## Issue Tracking with bd (beads)

This project uses **bd (beads)** for ALL issue tracking. See AGENTS.md template in Amp system prompt for bd workflow and commands.

## Proactive Tool Usage

**IMPORTANT:** Agents MUST use these tools proactively, not just when explicitly asked.

### Before Starting Any Task

1. **Check for prior solutions:** `cass search "<topic>" --robot --limit 5`
   - Look for similar problems already solved by any agent
   - Reuse patterns and avoid reinventing solutions

2. **Get playbook guidance:** `cm context "<task description>" --json`
   - Retrieve relevant rules from the learning playbook
   - Apply proven patterns for this type of work

3. **Check ready work:** `bd ready`
   - See what issues are unblocked and available
   - Claim work with `bd update <id> --status=in_progress`

### During Implementation

- **Track ALL work in beads** — never use markdown TODOs or other tracking
- **Create issues for discovered work:** `bd create --title="..." --type=task`
- **Update status as you progress:** `bd update <id> --status=...`

### After Completing Work

1. **Close completed issues:** `bd close <id> --reason="..."`
2. **Provide playbook feedback:** `cm mark <bullet-id> helpful` or `harmful`
3. **Sync before committing:** `bd sync --from-main`

---

For detailed agent workflows, MCP integration, and advanced features, refer to the Amp system instructions or bd documentation.

---

🔎 cass — Search All Your Agent History

What: cass indexes conversations from Claude Code, Codex, Cursor, Gemini, Aider, ChatGPT, and more into a unified, searchable index. Before solving a problem from scratch, check if any agent already solved something similar.

**WARNING:** NEVER run bare `cass` — it launches an interactive TUI. Always use `--robot` or `--json`.

Quick Start

```bash
# Check if index is healthy (exit 0=ok, 1=run index first)
cass health --json

# Search across all agent histories
cass search "authentication error" --robot --limit 5

# View a specific result (from search output)
cass view /path/to/session.jsonl -n 42 --json

# Expand context around a line
cass expand /path/to/session.jsonl -n 42 -C 3 --json

# Learn the full API
cass capabilities --json      # Feature discovery
cass robot-docs guide         # LLM-optimized docs
```

Key Flags
| Flag | Purpose |
|------|---------|
| `--robot` / `--json` | Machine-readable JSON output (required!) |
| `--fields minimal` | Reduce payload: source_path, line_number, agent only |
| `--limit N` | Cap result count |
| `--agent NAME` | Filter to specific agent (claude, codex, cursor, etc.) |
| `--days N` | Limit to recent N days |

stdout = data only, stderr = diagnostics. Exit 0 = success.

---

🧠 cm (cass-memory) — Learning Playbook for Agents

What: cass-memory is a reflection and memory system that learns from past sessions. It builds a "playbook" of rules that worked (or didn't) and provides context-relevant guidance for new tasks.

Quick Start

```bash
# Get relevant rules for your current task
cm context "implementing authentication" --json

# After completing a task, provide feedback on rules that helped/hurt
cm mark bullet-123 helpful      # This rule was useful
cm mark bullet-456 harmful      # This rule caused problems

# Check playbook health
cm doctor --json

# See most effective rules
cm top 10 --json

# Get self-documentation for agents
cm quickstart --json
```

Key Commands
| Command | Purpose |
|---------|---------|
| `cm context <task>` | Get relevant playbook rules for a task |
| `cm mark <id> helpful/harmful` | Provide feedback on a rule |
| `cm stats` | Show playbook health metrics |
| `cm top [N]` | Show most effective bullets |
| `cm doctor` | Check system health |
| `cm project` | Export playbook for documentation |

Workflow Integration

1. At task start: `cm context "your task description" --json` to get relevant guidance
2. During work: Apply relevant rules from the playbook
3. After completion: Use `cm mark` to record which rules helped or hurt
4. The system learns and improves recommendations over time

MCP Server
cm provides an MCP server for direct agent integration:

```bash
cm serve --port 8765
```

Project MCP config (`.mcp.json`) is pre-configured for port 8766.

---

Robot Interface (CLI) — Automation Guide for AI Agents
cass is designed to be maximally forgiving for AI agents. When your intent is clear, the CLI will auto-correct minor syntax issues and proceed with your command—while teaching you the proper syntax for future use. When intent is unclear, it provides rich, contextual error messages with examples.

Philosophy: Intent Over Syntax
Core principle: If we can reliably determine what you're trying to do, we do it. We then tell you how to do it correctly next time.

This means:

Minor syntax errors are auto-corrected: Single-dash long flags, wrong case, subcommand aliases
Correction notices teach proper syntax: Every correction includes an explanation
Failures include contextual examples: Based on what you were likely trying to do
Auto-Correction Features
The CLI applies multiple normalization layers before parsing:

Mistake Correction Note
-robot --robot Long flags need double-dash
-limit 5 --limit 5 Long flags need double-dash
--Robot, --LIMIT --robot, --limit Flags are lowercase
find "query" search "query" find is an alias for search
query "text" search "text" query is an alias for search
ls stats ls/list are aliases for stats
--robot-docs robot-docs It's a subcommand, not a flag
docs commands robot-docs commands docs is an alias
Flag after subcommand Moved to front Global flags are hoisted
Full alias list:

Search: find, query, q, lookup, grep → search
Stats: ls, list, info, summary → stats
Status: st, state → status
Index: reindex, idx, rebuild → index
View: show, get, read → view
Diag: diagnose, debug, check → diag
Capabilities: caps, cap → capabilities
Introspect: inspect, intro → introspect
Robot-docs: docs, help-robot, robotdocs → robot-docs
Correction Notices
When commands are auto-corrected, you'll receive a JSON notice on stderr (in robot mode):

{
"type": "syntax_correction",
"message": "Your command was auto-corrected. Please use the canonical form in future requests.",
"corrections": [
"'-robot' → '--robot' (use double-dash for long flags)",
"'find' → 'search' (canonical subcommand name)"
],
"tip": "Run 'cass robot-docs' for complete syntax documentation."
}
Important: The command still executes successfully—this is informational for learning.

Error Messages (When Intent Is Unclear)
If cass cannot determine your intent, it returns a rich error with:

Detected intent: What it thinks you were trying to do
Contextual examples: Relevant to your apparent goal
Specific hints: Based on mistakes detected in your command
Common mistakes: Wrong→Correct mappings for similar commands
Example Error Output (robot mode):

{
"status": "error",
"error": "error: unrecognized subcommand 'foobar'",
"kind": "argument_parsing",
"examples": [
"cass search \"error handling\" --robot --limit 10",
"cass search \"authentication\" --robot --agent claude"
],
"hints": [
"Use '--robot' (double-dash), not '-robot'",
"Use the 'search' subcommand explicitly: cass search \"your query\" --robot"
],
"common_mistakes": [
{"wrong": "cass query=\"foo\" --robot", "correct": "cass search \"foo\" --robot"},
{"wrong": "cass -robot find error", "correct": "cass search \"error\" --robot"}
],
"flag_syntax": {
"correct": ["--limit 5", "--robot", "--json"],
"incorrect": ["-limit 5", "limit=5", "--Limit"]
}
}
Quick Syntax Reference

# Correct syntax patterns

cass search "query" --robot --limit 10
cass search "query" --robot --agent claude --workspace /path
cass stats --robot
cass view <session-id> --robot --full
cass robot-docs commands
cass capabilities --json
cass health --json # Pre-flight check (<50ms, exit 0=healthy, 1=unhealthy)

# Common mistakes (all auto-corrected)

cass -robot search "query" → cass search "query" --robot
cass --Robot search "query" → cass search "query" --robot
cass find "query" --json → cass search "query" --json
cass --robot-docs → cass robot-docs
cass search "query" limit=5 → cass search "query" --limit 5
Best Practices for Agents
Always use --robot or --json: Structured output, diagnostics to stderr only
Read correction notices: Learn proper syntax to avoid corrections next time
Trust error messages: They're contextual and include working examples
Use cass robot-docs: Complete documentation optimized for LLM consumption
Start with cass capabilities --json: Discover all available commands and options
Pre-Flight Health Check
Before making complex queries, use the health command to verify cass is working:

cass health --json
Returns in <50ms:

Exit 0: Healthy—proceed with queries
Exit 1: Unhealthy—run cass index --full first
JSON output (healthy):

{"healthy": true, "latency_ms": 3, "state": {"index": {"exists": true, "fresh": true, ...}, "database": {...}, "pending": {...}}}
When unhealthy (exit 1), includes detailed state for diagnosis:

{"healthy": false, "latency_ms": 5, "state": {"index": {"exists": false, ...}, "database": {...}, "pending": {...}}}
The state object mirrors the cass state --json output, providing index freshness, database stats, and pending session count.

Recommended agent workflow:

cass health --json → Check exit code
If exit 1: cass index --full → Build index
Proceed with cass search "query" --robot
Exit Codes
Code Meaning Retryable
0 Success N/A
1 Health check failed (unhealthy) Yes—run cass index --full
2 Usage/parsing error No—fix syntax
3 Missing index Yes—run cass index first
9 Unknown error Maybe
Prepared blurb you can paste into other agents' guides (AGENTS.md / CLAUDE.md)

cass (Coding Agent Session Search) — CLI/TUI to search local agent histories.

Pre-flight: cass health --json (<50ms, exit 0=OK, 1=needs index).
Robot mode: cass --robot-help (automation contract) and cass robot-docs <topic> for focused docs.
JSON search: cass search "query" --robot [--limit N --offset N --agent codex --workspace /path].
Inspect hits: cass view <source_path> -n <line> --json.
Index first: cass index --full (or cass index --watch).
stdout=data only; stderr=diagnostics; exit codes stable (see --robot-help).
Non-TTY automation won't start TUI unless you explicitly run cass tui.

---

ast-grep vs ripgrep (quick guidance)
Use ast-grep when structure matters. It parses code and matches AST nodes, so results ignore comments/strings, understand syntax, and can safely rewrite code.

Refactors/codemods: rename APIs, change import forms, rewrite call sites or variable kinds.
Policy checks: enforce patterns across a repo (scan with rules + test).
Editor/automation: LSP mode; --json output for tooling.
Use ripgrep when text is enough. It's the fastest way to grep literals/regex across files.

Recon: find strings, TODOs, log lines, config values, or non‑code assets.
Pre-filter: narrow candidate files before a precise pass.
Rule of thumb

Need correctness over speed, or you'll apply changes → start with ast-grep.
Need raw speed or you're just hunting text → start with rg.
Often combine: rg to shortlist files, then ast-grep to match/modify with precision.
Snippets

Find structured code (ignores comments/strings):

ast-grep run -l TypeScript -p 'import $X from "$P"'
Codemod (only real var declarations become let):

ast-grep run -l JavaScript -p 'var $A = $B' -r 'let $A = $B' -U
Quick textual hunt:

rg -n 'console\.log\(' -t js
Combine speed + precision:

rg -l -t ts 'useQuery\(' | xargs ast-grep run -l TypeScript -p 'useQuery($A)' -r 'useSuspenseQuery($A)' -U
Mental model

Unit of match: ast-grep = node; rg = line.
False positives: ast-grep low; rg depends on your regex.
Rewrites: ast-grep first-class; rg requires ad‑hoc sed/awk and risks collateral edits.

---

Morph Warp Grep — AI-powered code search
Use mcp**morph-mcp**warp_grep for exploratory "how does X work?" questions. An AI search agent automatically expands your query into multiple search patterns, greps the codebase, reads relevant files, and returns precise line ranges with full context—all in one call.

Use ripgrep (via Grep tool) for targeted searches. When you know exactly what you're looking for—a specific function name, error message, or config key—ripgrep is faster and more direct.

Use ast-grep for structural code patterns. When you need to match/rewrite AST nodes while ignoring comments/strings, or enforce codebase-wide rules.

When to use what

Scenario Tool Why
"How is authentication implemented?" warp_grep Exploratory; don't know where to start
"Where is the L3 Guardian appeals system?" warp_grep Need to understand architecture, find multiple related files
"Find all uses of useQuery(" ripgrep Targeted literal search
"Find files with console.log" ripgrep Simple pattern, known target
"Rename getUserById → fetchUser" ast-grep Structural refactor, avoid comments/strings
"Replace all var with let" ast-grep Codemod across codebase
warp_grep strengths

Reduces context pollution: Returns only relevant line ranges, not entire files.
Intelligent expansion: Turns "appeals system" into searches for appeal, Appeals, guardian, L3, etc.
One-shot answers: Finds the 3-5 most relevant files with precise locations vs. manual grep→read cycles.
Natural language: Works well with "how", "where", "what" questions.
warp_grep usage

mcp**morph-mcp**warp_grep(
repoPath: "/data/projects/communitai",
query: "How is the L3 Guardian appeals system implemented?"
)
Returns structured results with file paths, line ranges, and extracted code snippets.

Rule of thumb

Don't know where to look → warp_grep (let AI find it)
Know the pattern → ripgrep (fastest)
Need AST precision → ast-grep (safest for rewrites)
Anti-patterns

❌ Using warp_grep to find a specific function name you already know → use ripgrep
❌ Using ripgrep to understand "how does X work" → wastes time with manual file reads
❌ Using ripgrep for codemods → misses comments/strings, risks collateral edits
Morph Warp Grep vs Standard Grep
Warp Grep = AI agent that greps, reads, follows connections, returns synthesized context with line numbers. Standard Grep = Fast regex match, you interpret results.

Decision: Can you write the grep pattern?

Yes → Grep
No, you have a question → mcp**morph-mcp**warp_grep
Warp Grep Queries (natural language, unknown location)
"How does the moderation appeals flow work?" "Where are websocket connections managed?" "What happens when a user submits a post?" "Where is rate limiting implemented?" "How does the auth session get validated on API routes?" "What services touch the moderationDecisions table?"

Standard Grep Queries (known pattern, specific target)
pattern="fileAppeal" # known function name pattern="class.*Service" # structural pattern pattern="TODO|FIXME|HACK" # markers pattern="processenv" path="apps/web" # specific string pattern="import.*from [']@/lib/db" # import tracing

What Warp Grep Does Internally
One query → 15-30 operations: greps multiple patterns → reads relevant sections → follows imports/references → returns focused line ranges (e.g., l3-guardian.ts:269-440) not whole files.

Anti-patterns
Don't Use Warp Grep For Why Use Instead
"Find function handleSubmit" Known name Grep pattern="handleSubmit"
"Read the auth config" Known file Read file_path="lib/auth/..."
"Check if X exists" Boolean answer Grep + check results
Quick lookups mid-task 5-10s latency Grep is 100ms
When Warp Grep Wins
Tracing data flow across files (API → service → schema → types)
Understanding unfamiliar subsystems before modifying
Answering "how" questions that span 3+ files
Finding all touching points for a cross-cutting concern

---

Do all work in the smallest coherent pieces possible. Commit after each change is made with a clear, concise (one to two lines) commit message.
Do not lump all work in a single commit.

````markdown
## UBS Quick Reference for AI Agents

UBS stands for "Ultimate Bug Scanner": **The AI Coding Agent's Secret Weapon: Flagging Likely Bugs for Fixing Early On**

**Install:** `curl -sSL https://raw.githubusercontent.com/Dicklesworthstone/ultimate_bug_scanner/main/install.sh | bash`

**Golden Rule:** `ubs <changed-files>` before every commit. Exit 0 = safe. Exit >0 = fix & re-run.

**Commands:**

```bash
ubs file.ts file2.py                    # Specific files (< 1s) — USE THIS
ubs $(git diff --name-only --cached)    # Staged files — before commit
ubs --only=js,python src/               # Language filter (3-5x faster)
ubs --ci --fail-on-warning .            # CI mode — before PR
ubs --help                              # Full command reference
ubs sessions --entries 1                # Tail the latest install session log
ubs .                                   # Whole project (ignores things like .venv and node_modules automatically)
```

**Output Format:**

```
⚠️  Category (N errors)
    file.ts:42:5 – Issue description
    💡 Suggested fix
Exit code: 1
```

Parse: `file:line:col` → location | 💡 → how to fix | Exit 0/1 → pass/fail

**Fix Workflow:**

1. Read finding → category + fix suggestion
2. Navigate `file:line:col` → view context
3. Verify real issue (not false positive)
4. Fix root cause (not symptom)
5. Re-run `ubs <file>` → exit 0
6. Commit

**Speed Critical:** Scope to changed files. `ubs src/file.ts` (< 1s) vs `ubs .` (30s). Never full scan for small edits.

**Bug Severity:**

- **Critical** (always fix): Null safety, XSS/injection, async/await, memory leaks
- **Important** (production): Type narrowing, division-by-zero, resource leaks
- **Contextual** (judgment): TODO/FIXME, console logs

**Anti-Patterns:**

- ❌ Ignore findings → ✅ Investigate each
- ❌ Full scan per edit → ✅ Scope to file
- ❌ Fix symptom (`if (x) { x.y }`) → ✅ Root cause (`x?.y`)
````

<!-- bv-agent-instructions-v1 -->

---

## Beads Workflow Integration

This project uses [beads_viewer](https://github.com/Dicklesworthstone/beads_viewer) for issue tracking. Issues are stored in `.beads/` and tracked in git.

### Essential Commands

```bash
# View issues (launches TUI - avoid in automated sessions)
bv

# CLI commands for agents (use these instead)
bd ready              # Show issues ready to work (no blockers)
bd list --status=open # All open issues
bd show <id>          # Full issue details with dependencies
bd create --title="..." --type=task --priority=2
bd update <id> --status=in_progress
bd close <id> --reason="Completed"
bd close <id1> <id2>  # Close multiple issues at once
bd sync               # Commit and push changes
```

### Workflow Pattern

1. **Start**: Run `bd ready` to find actionable work
2. **Claim**: Use `bd update <id> --status=in_progress`
3. **Work**: Implement the task
4. **Complete**: Use `bd close <id>`
5. **Sync**: Always run `bd sync` at session end

### Key Concepts

- **Dependencies**: Issues can block other issues. `bd ready` shows only unblocked work.
- **Priority**: P0=critical, P1=high, P2=medium, P3=low, P4=backlog (use numbers, not words)
- **Types**: task, bug, feature, epic, question, docs
- **Blocking**: `bd dep add <issue> <depends-on>` to add dependencies

### Session Protocol

**Before ending any session, run this checklist:**

```bash
git status              # Check what changed
git add <files>         # Stage code changes
bd sync                 # Commit beads changes
git commit -m "..."     # Commit code
bd sync                 # Commit any new beads changes
git push origin staging # Push to staging for preview deployment
```

### Best Practices

- Check `bd ready` at session start to find available work
- Update status as you work (in_progress → closed)
- Create new issues with `bd create` when you discover tasks
- Use descriptive titles and set appropriate priority/type
- Always `bd sync` before ending session

<!-- end-bv-agent-instructions -->

## Landing the Plane (Session Completion)

**When ending a work session**, you MUST complete ALL steps below. Work is NOT complete until `git push` succeeds.

**MANDATORY WORKFLOW:**

1. **File issues for remaining work** - Create issues for anything that needs follow-up
2. **Run quality gates** (if code changed) - Tests, linters, builds
3. **Update issue status** - Close finished work, update in-progress items
4. **PUSH TO STAGING** - This is MANDATORY:
   ```bash
   git pull origin staging --rebase
   bd sync
   git push origin staging
   git status  # MUST show "up to date with origin/staging"
   ```
5. **Clean up** - Clear stashes, prune remote branches
6. **Verify** - All changes committed AND pushed
7. **Hand off** - Provide context for next session

**CRITICAL RULES:**

- Work is NOT complete until `git push` succeeds
- NEVER stop before pushing - that leaves work stranded locally
- NEVER say "ready to push when you are" - YOU must push
- If push fails, resolve and retry until it succeeds

<!-- MCP_AGENT_MAIL_AND_BEADS_SNIPPET_START -->

## MCP Agent Mail: coordination for multi-agent workflows

What it is

- A mail-like layer that lets coding agents coordinate asynchronously via MCP tools and resources.
- Provides identities, inbox/outbox, searchable threads, and advisory file reservations, with human-auditable artifacts in Git.

Why it's useful

- Prevents agents from stepping on each other with explicit file reservations (leases) for files/globs.
- Keeps communication out of your token budget by storing messages in a per-project archive.
- Offers quick reads (`resource://inbox/...`, `resource://thread/...`) and macros that bundle common flows.

How to use effectively

1. Same repository
   - Register an identity: call `ensure_project`, then `register_agent` using this repo's absolute path as `project_key`.
   - Reserve files before you edit: `file_reservation_paths(project_key, agent_name, ["src/**"], ttl_seconds=3600, exclusive=true)` to signal intent and avoid conflict.
   - Communicate with threads: use `send_message(..., thread_id="FEAT-123")`; check inbox with `fetch_inbox` and acknowledge with `acknowledge_message`.
   - Read fast: `resource://inbox/{Agent}?project=<abs-path>&limit=20` or `resource://thread/{id}?project=<abs-path>&include_bodies=true`.
   - Tip: set `AGENT_NAME` in your environment so the pre-commit guard can block commits that conflict with others' active exclusive file reservations.

2. Across different repos in one project (e.g., Next.js frontend + FastAPI backend)
   - Option A (single project bus): register both sides under the same `project_key` (shared key/path). Keep reservation patterns specific (e.g., `frontend/**` vs `backend/**`).
   - Option B (separate projects): each repo has its own `project_key`; use `macro_contact_handshake` or `request_contact`/`respond_contact` to link agents, then message directly. Keep a shared `thread_id` (e.g., ticket key) across repos for clean summaries/audits.

Macros vs granular tools

- Prefer macros when you want speed or are on a smaller model: `macro_start_session`, `macro_prepare_thread`, `macro_file_reservation_cycle`, `macro_contact_handshake`.
- Use granular tools when you need control: `register_agent`, `file_reservation_paths`, `send_message`, `fetch_inbox`, `acknowledge_message`.

Common pitfalls

- "from_agent not registered": always `register_agent` in the correct `project_key` first.
- "FILE_RESERVATION_CONFLICT": adjust patterns, wait for expiry, or use a non-exclusive reservation when appropriate.
- Auth errors: if JWT+JWKS is enabled, include a bearer token with a `kid` that matches server JWKS; static bearer is used only when JWT is disabled.

## Integrating with Beads (dependency-aware task planning)

Beads provides a lightweight, dependency-aware issue database and a CLI (`bd`) for selecting "ready work," setting priorities, and tracking status. It complements MCP Agent Mail's messaging, audit trail, and file-reservation signals. Project: [steveyegge/beads](https://github.com/steveyegge/beads)

Recommended conventions

- **Single source of truth**: Use **Beads** for task status/priority/dependencies; use **Agent Mail** for conversation, decisions, and attachments (audit).
- **Shared identifiers**: Use the Beads issue id (e.g., `bd-123`) as the Mail `thread_id` and prefix message subjects with `[bd-123]`.
- **Reservations**: When starting a `bd-###` task, call `file_reservation_paths(...)` for the affected paths; include the issue id in the `reason` and release on completion.

Typical flow (agents)

1. **Pick ready work** (Beads)
   - `bd ready --json` → choose one item (highest priority, no blockers)
2. **Reserve edit surface** (Mail)
   - `file_reservation_paths(project_key, agent_name, ["src/**"], ttl_seconds=3600, exclusive=true, reason="bd-123")`
3. **Announce start** (Mail)
   - `send_message(..., thread_id="bd-123", subject="[bd-123] Start: <short title>", ack_required=true)`
4. **Work and update**
   - Reply in-thread with progress and attach artifacts/images; keep the discussion in one thread per issue id
5. **Complete and release**
   - `bd close bd-123 --reason "Completed"` (Beads is status authority)
   - `release_file_reservations(project_key, agent_name, paths=["src/**"])`
   - Final Mail reply: `[bd-123] Completed` with summary and links

Mapping cheat-sheet

- **Mail `thread_id`** ↔ `bd-###`
- **Mail subject**: `[bd-###] …`
- **File reservation `reason`**: `bd-###`
- **Commit messages (optional)**: include `bd-###` for traceability

Event mirroring (optional automation)

- On `bd update --status blocked`, send a high-importance Mail message in thread `bd-###` describing the blocker.
- On Mail "ACK overdue" for a critical decision, add a Beads label (e.g., `needs-ack`) or bump priority to surface it in `bd ready`.

Pitfalls to avoid

- Don't create or manage tasks in Mail; treat Beads as the single task queue.
- Always include `bd-###` in message `thread_id` to avoid ID drift across tools.

<!-- MCP_AGENT_MAIL_AND_BEADS_SNIPPET_END -->
