// The Ask AI composer's helpers, without React: the suggested prompts for the page in view, the slash commands, and the
// "@" mentions (what the menu looks for, what goes in the text, what is sent). The server resolves each mention again
// from its id with the member's own access; the label here is only the user's own text.

/** A prompt the empty panel (and the row above the composer) offers for the page in view. */
export interface Suggestion {
  label: string
  prompt: string
}

type PageKind = 'task' | 'tasks' | 'project' | 'doc' | 'docs' | 'chat' | 'activity' | 'other'

/** What kind of page a path shows, the way the router reads it (only the path, never the page's content). */
export function pageKind(path: string): PageKind {
  const [route = ''] = path.split(/[?#]/)
  const parts = route.split('/').filter(Boolean)
  const query = new URLSearchParams(path.includes('?') ? path.slice(path.indexOf('?') + 1).split('#')[0] : '')
  const [first, second, third] = parts
  if (first === 'tasks') {
    if (second === 'projects') return 'project'
    if (second && second !== 'pulls') return 'task'
    return query.get('project') ? 'project' : 'tasks'
  }
  if (first === 'views') return third ? 'task' : 'tasks'
  if (first === 'docs') return second && !['trash', 'import'].includes(second) ? 'doc' : 'docs'
  if (first === 'chat') return 'chat'
  if (first === 'activity' || first === 'inbox') return 'activity'
  return 'other'
}

const SUGGESTIONS: Record<PageKind, Suggestion[]> = {
  task: [
    { label: 'Summarize this task', prompt: 'Summarize this task: what it is about, where it stands and what is left.' },
    { label: 'What is blocking this?', prompt: 'What is blocking this task? Check its relations, comments and sub-issues.' },
    { label: 'Draft next steps as sub-issues', prompt: 'Draft the next steps for this task as sub-issues. Show them to me before you create anything.' },
  ],
  tasks: [
    { label: 'Summarize open tasks', prompt: 'Summarize the open tasks of my projects.' },
    { label: 'What is overdue?', prompt: 'Which open tasks are overdue? List them with their due dates and assignees.' },
    { label: 'Plan this week', prompt: 'Help me plan this week: which open tasks should I pick up first, and why?' },
    { label: 'What is assigned to me?', prompt: 'What is assigned to me?' },
  ],
  project: [
    { label: 'Summarize this project', prompt: 'Summarize the open tasks of this project.' },
    { label: 'What is overdue?', prompt: 'Which open tasks of this project are overdue? List them with their due dates and assignees.' },
    { label: 'Plan this week', prompt: 'Help me plan this week for this project: which open tasks come first, and why?' },
  ],
  doc: [
    { label: 'Summarize this doc', prompt: 'Summarize this doc in a few short points.' },
    { label: 'Turn this into tasks', prompt: 'Turn the action points of this doc into tasks. Show them to me before you create anything.' },
    { label: 'What is still open here?', prompt: 'Which questions or decisions in this doc are still open?' },
  ],
  docs: [
    { label: 'Open my recent docs', prompt: 'List the docs I can see, the most recently updated first.' },
    { label: 'Find a doc', prompt: 'Help me find a doc about ' },
    { label: 'Draft a new doc', prompt: 'Draft a new doc about ' },
  ],
  chat: [
    { label: 'Search the chat', prompt: 'Search the chat for messages about ' },
    { label: 'What is assigned to me?', prompt: 'What is assigned to me?' },
    { label: 'Summarize open tasks', prompt: 'Summarize the open tasks of my projects.' },
  ],
  activity: [
    { label: 'What needs my attention?', prompt: 'What needs my attention today? Look at my notifications and the tasks assigned to me.' },
    { label: 'What is assigned to me?', prompt: 'What is assigned to me?' },
    { label: 'What is overdue?', prompt: 'Which of my open tasks are overdue?' },
  ],
  other: [
    { label: 'What is assigned to me?', prompt: 'What is assigned to me?' },
    { label: 'Summarize open tasks', prompt: 'Summarize the open tasks of my projects.' },
    { label: 'Create a task', prompt: 'Create a task: ' },
    { label: 'Open my docs', prompt: 'Open my docs' },
  ],
}

/** The 3–4 prompts that fit the page in view. */
export function suggestionsFor(path: string): Suggestion[] {
  return SUGGESTIONS[pageKind(path)]
}

/** A prompt that ends in a space waits for the user to finish it; the others are sent as they are. */
export function isOpenPrompt(prompt: string): boolean {
  return prompt.endsWith(' ')
}

// ---- slash commands ----

export interface SlashCommand {
  name: string
  /** What the menu row says. */
  label: string
  /** The prompt the command writes, or an action of the panel. */
  run: { prompt: (path: string) => string } | { action: 'new_chat' }
}

const THIS_PAGE: Record<PageKind, string> = {
  task: 'this task',
  tasks: 'the open tasks of my projects',
  project: 'this project',
  doc: 'this doc',
  docs: 'my docs',
  chat: 'this chat',
  activity: 'my notifications',
  other: 'the open tasks of my projects',
}

export const SLASH_COMMANDS: SlashCommand[] = [
  { name: 'summarize', label: 'Summarize this page', run: { prompt: (path) => `Summarize ${THIS_PAGE[pageKind(path)]} in a few short points.` } },
  {
    name: 'plan',
    label: 'Plan my week',
    run: { prompt: () => 'Help me plan this week: which open tasks assigned to me come first, what is overdue, and what can wait?' },
  },
  {
    name: 'standup',
    label: 'What changed since yesterday',
    run: { prompt: () => 'Write my standup: what changed in my tasks since yesterday, what I work on next, and what is blocked.' },
  },
  { name: 'new', label: 'Start a new chat', run: { action: 'new_chat' } },
]

/** The command name being typed: the draft is "/" and letters, with the cursor at its end. Null otherwise. */
export function slashQuery(draft: string, cursor: number): string | null {
  const match = /^\/([a-z]*)$/i.exec(draft)
  if (!match || cursor !== draft.length) return null
  return match[1]!.toLowerCase()
}

export function matchingCommands(query: string): SlashCommand[] {
  return SLASH_COMMANDS.filter((command) => command.name.startsWith(query))
}

/** The prompt a command writes. Text after the command ("/summarize in Dutch") is added to it. */
export function commandPrompt(command: SlashCommand, path: string, rest = ''): string | null {
  if (!('prompt' in command.run)) return null
  const extra = rest.trim()
  return extra ? `${command.run.prompt(path)} ${extra}` : command.run.prompt(path)
}

/** A sent "/command …": the command and the text after it. Unknown commands are sent as plain text. */
export function parseSlash(text: string): { command: SlashCommand; rest: string } | null {
  const match = /^\/([a-z]+)(?:\s+([\s\S]*))?$/i.exec(text.trim())
  if (!match) return null
  const command = SLASH_COMMANDS.find((candidate) => candidate.name === match[1]!.toLowerCase())
  return command ? { command, rest: match[2] ?? '' } : null
}

// ---- mentions ----

export type MentionKind = 'task' | 'project' | 'doc' | 'person'

/** A mention in the composer: the item's id and the text that names it in the message (`@ENG-12`, `@Roadmap`). */
export interface Mention {
  kind: MentionKind
  id: string
  label: string
}

/** At most this many mentions go with one message; the server keeps the same limit. */
export const MAX_MENTIONS = 10

/**
 * The "@query" before the cursor, or null. "@" starts a mention after a space or at the start only (not in mail@x).
 * A mention already picked (`labels`) is not a query: the menu does not open again behind it.
 */
export function mentionQuery(text: string, cursor: number, labels: readonly string[] = []): { start: number; query: string } | null {
  const before = text.slice(0, cursor)
  const at = before.lastIndexOf('@')
  if (at === -1) return null
  if (at > 0 && !/\s/.test(before[at - 1]!)) return null
  const query = before.slice(at + 1)
  if (labels.some((label) => query.startsWith(label) && /^\s|^$/.test(query.slice(label.length)))) return null
  if (query.includes('\n') || query.length > 48 || /^\s/.test(query) || query.trim().split(/\s+/).length > 4) return null
  return { start: at, query }
}

/** The label a mention gets in the text: a task's key, otherwise its name, on one line. */
export function mentionLabel(kind: MentionKind, name: string, identifier?: string): string {
  const label = kind === 'task' && identifier ? identifier : name
  return label.replace(/\s+/g, ' ').trim().slice(0, 80) || 'Untitled'
}

/** The text with "@label " in place of the typed "@query", and where the cursor goes. */
export function insertMention(text: string, start: number, cursor: number, label: string): { text: string; cursor: number } {
  const insertion = `@${label} `
  return { text: `${text.slice(0, start)}${insertion}${text.slice(cursor)}`, cursor: start + insertion.length }
}

/**
 * Where "@label" stands in the text as a whole mention: after the start or a space, and before a space, the end or a
 * punctuation mark. So "@ENG-1" is not found inside "@ENG-12". -1 when it is not there.
 */
export function findMention(text: string, label: string, from = 0): number {
  const needle = `@${label}`
  for (let index = text.indexOf(needle, from); index !== -1; index = text.indexOf(needle, index + 1)) {
    const before = index === 0 ? '' : text[index - 1]!
    const after = text[index + needle.length] ?? ''
    if ((before === '' || /\s/.test(before)) && (after === '' || /[\s.,!?;:)\]}"']/.test(after))) return index
  }
  return -1
}

/** The text without one "@label" mention (and the space after it); the cursor goes where it stood. */
export function removeMention(text: string, label: string): { text: string; cursor: number } {
  const index = findMention(text, label)
  if (index === -1) return { text, cursor: text.length }
  let end = index + label.length + 1
  if (text[end] === ' ') end += 1
  return { text: `${text.slice(0, index)}${text.slice(end)}`, cursor: index }
}

/** The mentions a message sends: those still named in its text, once each, at most MAX_MENTIONS. */
export function mentionsToSend(text: string, mentions: readonly Mention[]): Mention[] {
  const seen = new Set<string>()
  const kept: Mention[] = []
  for (const mention of mentions) {
    const key = `${mention.kind}:${mention.id}`
    if (seen.has(key) || findMention(text, mention.label) === -1) continue
    seen.add(key)
    kept.push(mention)
    if (kept.length === MAX_MENTIONS) break
  }
  return kept
}

/** What goes to the server: kinds and ids only (it never uses a label from the client for the model). */
export function serializeMentions(mentions: readonly Mention[]): string {
  return JSON.stringify(mentions.map(({ kind, id, label }) => ({ kind, id, token: label })))
}

export type TextSegment = { text: string; mention?: MentionKind }

/** A sent message split into plain text and its "@label" mentions (all plain text: the renderer adds no markup). */
export function mentionSegments(text: string, mentions: readonly { kind: MentionKind; token: string }[]): TextSegment[] {
  const tokens = mentions
    .filter((mention) => mention.token)
    .map((mention) => ({ kind: mention.kind, needle: `@${mention.token}` }))
    // the longer label first, so "@Roadmap 2027" is not cut at "@Roadmap"
    .sort((a, b) => b.needle.length - a.needle.length)
  const segments: TextSegment[] = []
  let rest = text
  while (rest) {
    let best: { index: number; kind: MentionKind; needle: string } | null = null
    for (const token of tokens) {
      const index = findMention(rest, token.needle.slice(1))
      if (index !== -1 && (best === null || index < best.index)) best = { index, ...token }
    }
    if (!best) {
      segments.push({ text: rest })
      break
    }
    if (best.index > 0) segments.push({ text: rest.slice(0, best.index) })
    segments.push({ text: best.needle, mention: best.kind })
    rest = rest.slice(best.index + best.needle.length)
  }
  return segments
}

/** The mentions a stored user message carries (`metadata.custom.mentions`, set by the server or by the composer). */
export function readMentions(value: unknown): { kind: MentionKind; token: string }[] {
  if (!Array.isArray(value)) return []
  return value.flatMap((entry) => {
    if (!entry || typeof entry !== 'object') return []
    const { kind, token } = entry as { kind?: unknown; token?: unknown }
    if (typeof token !== 'string' || !token || (kind !== 'task' && kind !== 'project' && kind !== 'doc' && kind !== 'person')) return []
    return [{ kind, token }]
  })
}
