# Ask AI

Ask AI is the in-app assistant. It runs on Anthropic's Claude models and acts as the signed-in member through the
same tools as the MCP server (`docs/mcp.md`), in-process. The code is in `apps/server/src/assistant/` and
`apps/web/src/features/assistant/`.

## Setup

- A workspace owner or admin opens **Settings → AI**, pastes an Anthropic API key, clicks **Test** and **Save**, and
  picks a model (Claude Sonnet 5.5 by default, Opus 5.5 or Haiku 5.5).
- The key is sealed with the app key (`ORBIT_APP_KEY`) for that workspace only and is never shown again: the page
  shows its last four characters. Without an app key the server refuses to store a key.
- `ORBIT_AI_ANTHROPIC_KEY` (optional) is used only by workspaces **without a key of their own**. On a server with
  several tenants every such workspace would spend it, so set it only on a single-tenant server. Such a workspace's
  daily token limit is at most `ORBIT_AI_MAX_DAILY_TOKENS` (default 2,000,000); its admins cannot raise it.
- `ORBIT_DEV_AI_ANTHROPIC_BASE_URL` points Ask AI at a stand-in for the Anthropic API (`scripts/assistant-fake-model.ts`).
  It is honoured only with `ORBIT_ENV=development` and an `http` loopback address (`127.0.0.1`, `::1`, `localhost`).

## Using it

- Open it with **Ask AI** in the sidebar (under Search) or **Ctrl+J** (**Cmd+J** on a Mac). On a desktop it is a
  column at the right of the page (Widen makes it half the window); on a phone it is a sheet.
- Each step shows what Ask AI does (reads, then changes). A change shows an approval card with its full arguments;
  technical values (versions, ids without a name) are folded under **Details**.
- A thin edge around the window and the panel shows what Ask AI is doing, with an icon and a status line at the top
  of the panel ("Reading your projects…", "Changing GEN-3…"): blue while it thinks or reads (a slow pulse, none with
  reduced motion), amber while an approved change runs, violet while a change waits for your approval. While it
  waits, the rest of the page dims a little but stays usable, and the tab title starts with "(Approval needed)". The
  colours are the `--assistant-*` variables in `apps/web/src/index.css`.
- **Follow along** (the eye button, on by default, saved per browser): the page goes to what Ask AI changed or
  opened. Off, the page stays and a "go there" link appears instead. The page never moves while you type in a field
  or have a dialog open.
- **Stop** ends the run at once; **New chat** starts over (the old conversation is deleted).
- The mobile app has **Settings → AI** (key, Test, Save, model, daily limit, usage); the chat panel itself is on the
  web only for now.

## What it may do

- Reads run at once. Every change waits for the member's approval of the exact arguments, which the server keeps
  (the client only answers approve or decline). The card shows text arguments as plain text: code fences and HTML
  stay visible, nothing is rendered.
- **Allow in this chat** skips the approval of that kind of change for the rest of the conversation, but only for
  structured changes (a status, a name, assignees, labels, dates, a project's settings). It is never offered for:
  - tools that overwrite, replace or move content (MCP's destructive flag);
  - changes that carry free text Orbit shows as Markdown or HTML elsewhere (comments, task and status
    descriptions, docs, doc comments, database cells). Which tools those are is derived from their argument names
    (`RICH_TEXT_ARGS` in `registry.rs`); a test makes every string argument of every change tool be classified, so a
    new tool cannot slip through.
  - An allowed change whose text still holds a link, an image or a diagram asks again (an extra signal only).
- After a change that ran without a card, nothing moves the page for the rest of that message (no follow along,
  no `open_page`); the step's link opens it.
- Not available to Ask AI: delete, purge, trash and empty-trash tools; every mail tool (mail reaches people outside
  the workspace, and its threads are outside content); file uploads and downloads; and project membership. Nothing
  sends anything outside the workspace. Every MCP tool has a label in `registry.rs` (a test fails for a new tool
  without one), so a new tool stays out until someone decides.
- Answers never render diagrams (a Mermaid block shows as code) or images from another site, and show the full
  address of every external link, so text the assistant read cannot make the browser send data elsewhere.
- Tool results, and the member's and workspace's names, reach the model marked as untrusted data.

## Limits

- Per message: 12 model steps, 20 tool calls; per run: 4 minutes. Stop and a closed tab stop it at once.
- Per member: 20 model requests a minute (shared by every server of the database).
- Per workspace: a daily token limit (default 2,000,000 per UTC day; cache reads count a tenth), set in Settings → AI.
  - It limits **tokens, not money**. At 2,000,000 tokens a day the cost lies roughly between all-input and
    all-output prices: Claude Haiku 5.5 about $0.20–$1, Sonnet 5.5 about $4–$20, Opus 5.5 about $8–$40 a day
    (list prices; your Anthropic invoice is what counts). Set a spend limit in the Anthropic Console as well.
  - Every model request is counted **before** it goes out, at an estimate (its input size plus the most output it
    may write), and settled afterwards to what it used. A request cut off after it was sent (Stop, time limit, closed
    tab, broken stream) keeps its whole output budget, because output and thinking before the stop are billed but
    not reported. The limit
    is approximate: the request that crosses it still finishes, so it can be passed by about one request per member.
- When the limits cannot be checked (a database error), no request goes out.
- Conversations are kept for 6 hours after their last use, for their member only. Token usage per request is kept
  for the usage table in Settings → AI.

## Usage and cost

Settings → AI shows, for today and the last 30 days: requests, new input, cache reads, cache writes and output
(thinking included), as Anthropic reported them, and a rough cost at list prices (`settings::rates`). Anthropic's
`input_tokens` is only the uncached remainder, so each kind of input is counted and priced on its own: cache reads
at a small part of the input price, 5-minute cache writes at 1.25 times input. Haiku 5.5 requests with a prompt over
100,000 tokens use its long-prompt rates. A request answered by a model without known prices is listed as "not
priced". Prices can change: the invoice is what counts.

## Audit

Changes made by Ask AI are audited as the member with request id `assistant` and `via: "assistant"`, plus an
`assistant.change` row (applied, declined or failed) per change. Settings changes write
`workspace.ai_settings_updated` (never the key).

## Notes

`ORBIT_AI_MAX_DAILY_TOKENS` that is not a positive whole number is ignored with a warning at startup.
