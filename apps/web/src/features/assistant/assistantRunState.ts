// What Ask AI is doing right now, in four states the whole app shows: a coloured edge around the page and the panel,
// a status line at the top of the panel, and a tab title prefix while a change waits. Everything here is derived from
// the panel's own run state (is a run streaming, which tool call is in flight, does a call wait for approval), so it
// never needs its own bookkeeping. The colours are CSS variables in index.css (`--assistant-*`).
import { fallbackRisk, parseToolResult } from './toolSteps'

/** idle: nothing runs. thinking: the model thinks or reads. changing: an approved change runs. waiting: a change
    waits for the user's approval. */
export type AssistantRunState = 'idle' | 'thinking' | 'changing' | 'waiting'

export type AssistantRun = {
  state: AssistantRunState
  /** The status line, e.g. "Reading your projects…" or "Changing GEN-3…"; '' when idle. */
  text: string
}

export const IDLE_RUN: AssistantRun = { state: 'idle', text: '' }

type Approval = { approved?: boolean; resolution?: string }
type Part = { type: string; toolName?: string; args?: unknown; result?: unknown; approval?: Approval }
type Message = { role: string; content: readonly Part[] }

function objectOf(value: unknown): Record<string, unknown> | null {
  return value !== null && typeof value === 'object' && !Array.isArray(value) ? (value as Record<string, unknown>) : null
}

function isWaiting(approval: Approval | undefined): boolean {
  return !!approval && approval.approved === undefined && !approval.resolution
}

/** A task identifier as Orbit shows it, e.g. GEN-3. */
const TASK_KEY = /^[A-Za-z][A-Za-z0-9]{0,9}-\d{1,7}$/

/** Task ids to their identifiers, from the tool results of this conversation (a list or a single task), so the
    status line says "Changing GEN-3…" and not an id. */
export function taskKeys(messages: readonly Message[]): Map<string, string> {
  const keys = new Map<string, string>()
  const visit = (value: unknown, depth: number) => {
    if (depth > 3 || keys.size > 2000) return
    if (Array.isArray(value)) {
      for (const item of value) visit(item, depth + 1)
      return
    }
    const record = objectOf(value)
    if (!record) return
    if (typeof record.id === 'string' && typeof record.identifier === 'string' && TASK_KEY.test(record.identifier)) {
      keys.set(record.id, record.identifier.toUpperCase())
    }
    for (const child of Object.values(record)) if (child !== null && typeof child === 'object') visit(child, depth + 1)
  }
  for (const message of messages) {
    if (message.role !== 'assistant') continue
    for (const part of message.content) {
      if (part.type === 'tool-call' && part.result !== undefined) visit(parseToolResult(part.result), 0)
    }
  }
  return keys
}

function clip(text: string, length: number): string {
  const line = text.replace(/\s+/g, ' ').trim()
  return line.length > length ? `${line.slice(0, length)}…` : line
}

/** What a call is about, in the user's terms: a task key, or a quoted title or name. '' when unknown. */
export function callSubject(args: unknown, keys?: ReadonlyMap<string, string>): string {
  const record = objectOf(args) ?? {}
  for (const name of ['task_id', 'identifier', 'key']) {
    const value = record[name]
    if (typeof value !== 'string') continue
    if (name !== 'key' && TASK_KEY.test(value)) return value.toUpperCase()
    const known = keys?.get(value)
    if (known) return known
  }
  for (const name of ['title', 'name']) {
    const value = record[name]
    if (typeof value === 'string' && value.trim()) return `“${clip(value, 40)}”`
  }
  return ''
}

const READING: Record<string, string> = {
  get_me: 'Checking your account…',
  list_projects: 'Reading your projects…',
  list_tasks: 'Reading your tasks…',
  get_task: 'Reading a task…',
  list_task_statuses: 'Reading your statuses…',
  list_labels: 'Reading your labels…',
  list_milestones: 'Reading your milestones…',
  list_cycles: 'Reading your cycles…',
  list_comments: 'Reading comments…',
  list_members: 'Looking up members…',
  list_docs: 'Reading your docs…',
  search_docs: 'Searching your docs…',
  get_doc: 'Reading a doc…',
  list_conversations: 'Reading your chats…',
  list_messages: 'Reading a chat…',
  get_thread: 'Reading a thread…',
  search_messages: 'Searching your chats…',
  get_database: 'Reading a database…',
  query_database: 'Reading a database…',
  list_views: 'Reading your views…',
  get_view: 'Reading a view…',
  list_notifications: 'Reading your Inbox…',
  open_page: 'Opening a page…',
  navigate: 'Opening a page…',
  load_tools: 'Getting ready…',
}

function changeVerb(tool: string): string {
  if (/^create_/.test(tool)) return 'Creating'
  if (/^(delete|trash|purge|empty)_/.test(tool)) return 'Deleting'
  if (/^restore_/.test(tool)) return 'Restoring'
  if (/^move_/.test(tool)) return 'Moving'
  return 'Changing'
}

/** The status line for a state, from the call in flight (if any). */
export function statusText(state: AssistantRunState, call?: { toolName: string; args?: unknown }, keys?: ReadonlyMap<string, string>): string {
  if (state === 'idle') return ''
  if (state === 'waiting') return 'Waiting for your approval'
  const subject = call ? callSubject(call.args, keys) : ''
  if (state === 'changing') {
    if (!call) return 'Making a change…'
    return subject ? `${changeVerb(call.toolName)} ${subject}…` : 'Making a change…'
  }
  if (!call) return 'Thinking…'
  if ((call.toolName === 'get_task' || call.toolName === 'get_doc') && subject) return `Reading ${subject}…`
  return READING[call.toolName] ?? 'Looking things up…'
}

/** The run state from the panel's thread: a waiting approval wins, then a change in flight, then any run. */
export function deriveRun({ running, messages }: { running: boolean; messages: readonly Message[] }): AssistantRun {
  let waiting = false
  for (const message of messages) {
    if (message.role !== 'assistant') continue
    if (message.content.some((part) => part.type === 'tool-call' && isWaiting(part.approval))) waiting = true
  }
  if (waiting) return { state: 'waiting', text: statusText('waiting') }
  if (!running) return IDLE_RUN
  const last = messages.at(-1)
  const call = last?.role === 'assistant' ? last.content.findLast((part) => part.type === 'tool-call' && part.result === undefined) : undefined
  if (!call?.toolName) return { state: 'thinking', text: statusText('thinking') }
  const keys = taskKeys(messages)
  // an approved call, or a change the user allowed for this chat, runs without a card
  const change = call.approval?.approved === true || fallbackRisk(call.toolName) !== 'read'
  const state: AssistantRunState = change ? 'changing' : 'thinking'
  return { state, text: statusText(state, { toolName: call.toolName, args: call.args }, keys) }
}

/** The tab title while a change waits, so it is noticed from another tab. */
export const TITLE_PREFIX = '(Approval needed) '

export function titleFor(title: string, state: AssistantRunState): string {
  const base = title.startsWith(TITLE_PREFIX) ? title.slice(TITLE_PREFIX.length) : title
  return state === 'waiting' ? `${TITLE_PREFIX}${base}` : base
}

/** Sets or removes the prefix on the document's title; only the prefix is touched, so a title a page set meanwhile
    stays. */
export function syncDocumentTitle(state: AssistantRunState, doc: Pick<Document, 'title'> = document): void {
  const next = titleFor(doc.title, state)
  if (next !== doc.title) doc.title = next
}

/** How the page edge moves: thinking pulses slowly, unless the user asked for less motion. */
export function edgeMotion(state: AssistantRunState, reducedMotion: boolean): 'none' | 'steady' | 'pulse' {
  if (state === 'idle') return 'none'
  return state === 'thinking' && !reducedMotion ? 'pulse' : 'steady'
}
