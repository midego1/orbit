# Ask AI

Ask AI is the in-app assistant. It runs on Anthropic's Claude models and acts as the signed-in member through the
same tools as the MCP server (`docs/mcp.md`), in-process. The code is in `apps/server/src/assistant/` and
`apps/web/src/features/assistant/`.

## Setup

- A workspace owner or admin opens **Settings → AI**, pastes an Anthropic API key, clicks **Test** and **Save**, and
  picks a model (Claude Sonnet 5.5 by default, Opus 5.5 or Haiku 5.5).
- **Models per chat:** the admin can also tick other models that members may pick per chat (column
  `workspace_ai_settings.allowed_models`; none by default, so every chat uses the workspace's model). A member picks
  one with `/model` or the model menu in the panel's header; the server stores the pick on the conversation
  (`assistant_conversations.model`) only when it is on the list, and every run checks it again. When an admin takes
  a model off the list, the chat's next run uses the workspace's model, clears the pick and tells the member once
  (AG-UI `CUSTOM` event `orbit.model_fallback`). The daily limit and usage count every model the same way, per model.
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
- The window edge and the panel show what Ask AI is doing, with an icon and a status line at the top of the panel
  ("Reading your projects…", "Changing GEN-3…"). While it thinks or reads, a blue light travels around the window edge
  and along the top of the status line, the icon moves and the text shimmers; while an approved change runs, the same
  happens in amber, a little faster; the step that runs now has a soft pulsing ring. While a change waits for your
  approval, everything stops: the edge is violet and steady, the rest of the page dims a little but stays usable, and
  the tab title starts with "(Approval needed)". With reduced motion nothing moves; the colours stay. Colours, sizes
  and timings are the `--assistant-*` block in `apps/web/src/index.css`.
- Results show as cards, built by the server from the tool's own result (`apps/server/src/assistant/cards.rs`), never
  from the model's text: a task (key, title, status, priority, assignees, due date; the card links to the task), a
  task list (exact counts, rows grouped by project or status, five rows until **Show all**), docs and projects. An
  approval for a task shows the task as it is now, read by the server when it asks, with each changed field as
  before → after (the old value struck through); a new task shows as a card. A message that made changes ends with
  "N changes applied" and a link per change. Every text in a card is plain text; every link is an in-app path built
  from an id and checked again in the panel.
- A task key in an answer (`GEN-1`) is a link with the task's title and status on hover, but only when a card of this
  conversation named that task; other keys, and keys in code, stay text.
- A task list the model sees carries `totals` (all, open, done, per project and per status), counted by Orbit over
  every task the call's filter matches (one grouped count with the same filter and access checks, not only the
  page), and the model is told to quote them instead of counting. The counts come first in what the model reads; when
  the tasks do not all fit, whole tasks are left out (`items_omitted`), never the counts.
- **Follow along** (the eye button, on by default, saved per browser): the page goes to what Ask AI changed or
  opened. Off, the page stays and a "go there" link appears instead. The page never moves while you type in a field
  or have a dialog open.
- **Suggestions:** an empty chat offers three or four prompts that fit the page in view (a task: "Summarize this
  task", "What is blocking this?", "Draft next steps as sub-issues"; a task list: "Summarize open tasks", "What is
  overdue?", "Plan this week"; a doc: "Summarize this doc", "Turn this into tasks"; other pages a general set). After
  an answer a small row of them sits above the field. They come from the page's path only
  (`apps/web/src/features/assistant/composer.ts`); a click sends the prompt, or fills the field when the prompt ends
  open ("Create a task: ").
- **Commands:** "/" at the start of the field opens a menu (↑/↓, Enter or Tab, Esc; the full prompt shows on hover):
  `/summarize` (this page), `/plan` (this week), `/standup` (what changed since yesterday) write a plain prompt into
  the field, which you can still edit; text after a typed command is added to it. `/new` starts a new chat. A command
  is only text: it runs like any other message, with the same approvals. `/model` (only when the workspace allows more
  than one model) lists the allowed models; `/model haiku` picks one at once. It is not sent to the model.
- **Mentions:** "@" opens a picker of people, tasks, projects and docs (the app's own searches). A pick writes
  `@ENG-12` (a task's key, otherwise the name) into the message and shows as a chip above the field. The message
  sends only each mention's kind and id (context entry `mentions`, at most 10, only those still named in the text).
  The server reads each one again through the same tools as the model, as the member
  (`apps/server/src/assistant/mentions.rs`): an id they cannot see, or that does not exist, is dropped. The model
  gets Orbit's own names and ids (`task "ENG-12" "Fix login" (task id …)`), marked as untrusted data; a name the
  client sent is never used. The reads run together before the first model step; Stop ends them, and after 10
  seconds the message goes without its mentions. The sent message shows its mentions as chips, as plain text.
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
