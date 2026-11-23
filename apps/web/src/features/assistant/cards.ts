// The cards of Ask AI: what the server built from a tool's own result (apps/server/src/assistant/cards.rs), checked
// again here. Every text is shown as plain text (never Markdown or HTML), and a link is only an in-app path to a task,
// doc or the task list; anything else gives no link.

import type { StatusCategory, TaskPriority } from '@/features/tasks/api/models'

export type CardStatus = { name: string; category: StatusCategory; color: string | null }

export type TaskCardData = {
  kind: 'task'
  key: string | null
  title: string
  /** `null` for the preview of a task that does not exist yet. */
  href: string | null
  status: CardStatus | null
  priority: TaskPriority
  assignees: { name: string }[]
  due: string | null
  project: { name: string; key: string } | null
}

export type Count = { name: string; key?: string; category?: string; count: number }

export type TasksCardData = {
  kind: 'tasks'
  totals: { tasks: number; open: number; done: number; byProject: Count[]; byStatus: Count[]; complete: boolean }
  items: TaskCardData[]
}

export type LinkItem = { title: string; href: string; updatedAt?: string | null; key?: string }
export type DocsCardData = { kind: 'docs'; items: LinkItem[] }
export type ProjectsCardData = { kind: 'projects'; items: LinkItem[] }

export type Change = { verb: string; key: string | null; title: string; href: string | null }
export type SummaryCardData = { kind: 'summary'; changes: Change[] }

export type CardData = TaskCardData | TasksCardData | DocsCardData | ProjectsCardData | SummaryCardData

export type FieldChange = { field: string; before: string; after: string }
export type ApprovalPreview = { kind: 'change'; task: TaskCardData; changes: FieldChange[] } | { kind: 'create'; task: TaskCardData }

/** The step the server adds at the end of a message that made changes; it carries the summary card only. */
export const SUMMARY_TOOL = 'orbit_summary'

const MAX_TEXT = 300
const CATEGORIES: readonly string[] = ['unstarted', 'started', 'completed', 'cancelled', 'duplicate']
const PRIORITIES: readonly string[] = ['none', 'low', 'medium', 'high', 'urgent']
const DONE_CATEGORIES: readonly string[] = ['completed', 'cancelled', 'duplicate']

function record(value: unknown): Record<string, unknown> | null {
  return value !== null && typeof value === 'object' && !Array.isArray(value) ? (value as Record<string, unknown>) : null
}

function list(value: unknown): unknown[] {
  return Array.isArray(value) ? value : []
}

function str(value: unknown, max = MAX_TEXT): string {
  return typeof value === 'string' ? value.slice(0, max) : ''
}

function count(value: unknown): number {
  return typeof value === 'number' && Number.isInteger(value) && value >= 0 ? value : 0
}

/** `GEN-12`: a project key and a number, as the server builds it. */
export function isTaskKey(value: unknown): value is string {
  return typeof value === 'string' && /^[A-Za-z0-9_]{1,20}-[1-9]\d{0,14}$/.test(value)
}

/**
 * The path of a card link: only `/tasks/<id>?workspace=<id>`, `/docs/<id>?workspace=<id>` or
 * `/tasks?project=<id>&workspace=<id>`, ids of letters, digits, `-` and `_`. Anything else (another origin, `//host`,
 * `javascript:`, other pages) is no link.
 */
export function cardPath(value: unknown): string | null {
  if (typeof value !== 'string') return null
  const id = '[A-Za-z0-9_-]{1,64}'
  const allowed = [
    new RegExp(`^/tasks/${id}\\?workspace=${id}$`),
    new RegExp(`^/docs/${id}\\?workspace=${id}$`),
    new RegExp(`^/tasks\\?project=${id}&workspace=${id}$`),
  ]
  return allowed.some((pattern) => pattern.test(value)) ? value : null
}

function hexColor(value: unknown): string | null {
  return typeof value === 'string' && /^#(?:[0-9a-fA-F]{3}|[0-9a-fA-F]{6}|[0-9a-fA-F]{8})$/.test(value) ? value : null
}

function day(value: unknown): string | null {
  return typeof value === 'string' && /^\d{4}-\d{2}-\d{2}$/.test(value) ? value : null
}

function status(value: unknown): CardStatus | null {
  const raw = record(value)
  if (!raw) return null
  const category = CATEGORIES.includes(raw.category as string) ? (raw.category as StatusCategory) : 'unstarted'
  return { name: str(raw.name, 80) || 'No status', category, color: hexColor(raw.color) }
}

export function isDone(task: Pick<TaskCardData, 'status'>): boolean {
  return !!task.status && DONE_CATEGORIES.includes(task.status.category)
}

function task(value: unknown, needsLink: boolean): TaskCardData | null {
  const raw = record(value)
  if (!raw) return null
  const href = cardPath(raw.href)
  if (needsLink && !href) return null
  const project = record(raw.project)
  return {
    kind: 'task',
    key: isTaskKey(raw.key) ? raw.key : null,
    title: str(raw.title),
    href,
    status: status(raw.status),
    priority: PRIORITIES.includes(raw.priority as string) ? (raw.priority as TaskPriority) : 'none',
    assignees: list(raw.assignees)
      .slice(0, 10)
      .map((person) => ({ name: str(record(person)?.name, 80) }))
      .filter((person) => person.name),
    due: day(raw.due),
    project: project ? { name: str(project.name, 80), key: str(project.key, 20) } : null,
  }
}

function counts(value: unknown): Count[] {
  return list(value)
    .map(record)
    .filter((entry): entry is Record<string, unknown> => entry !== null)
    .map((entry) => ({ name: str(entry.name, 80), key: str(entry.key, 20) || undefined, category: str(entry.category, 20) || undefined, count: count(entry.count) }))
}

function links(value: unknown): LinkItem[] {
  return list(value)
    .map(record)
    .flatMap((entry) => {
      const href = cardPath(entry?.href)
      if (!entry || !href) return []
      const updatedAt = typeof entry.updatedAt === 'string' && !Number.isNaN(Date.parse(entry.updatedAt)) ? entry.updatedAt : null
      return [{ title: str(entry.title) || str(entry.name) || 'Untitled', href, updatedAt, key: str(entry.key, 20) || undefined }]
    })
}

/** The card a step's result carries (`{…, "card": {…}}`), checked. `null` when it has none or it is not valid. */
export function readCard(result: unknown): CardData | null {
  const card = record(record(result)?.card)
  if (!card) return null
  switch (card.kind) {
    case 'task':
      return task(card, true)
    case 'tasks': {
      const totals = record(card.totals) ?? {}
      return {
        kind: 'tasks',
        totals: {
          tasks: count(totals.tasks),
          open: count(totals.open),
          done: count(totals.done),
          byProject: counts(totals.byProject),
          byStatus: counts(totals.byStatus),
          complete: totals.complete !== false,
        },
        items: list(card.items).flatMap((item) => task(item, true) ?? []),
      }
    }
    case 'docs':
    case 'projects': {
      const items = links(card.items)
      return items.length ? { kind: card.kind, items } : null
    }
    case 'summary': {
      const changes = list(card.changes)
        .map(record)
        .flatMap((entry) =>
          entry ? [{ verb: str(entry.verb, 40) || 'Changed', key: isTaskKey(entry.key) ? entry.key : null, title: str(entry.title), href: cardPath(entry.href) }] : [],
        )
      return changes.length ? { kind: 'summary', changes } : null
    }
    default:
      return null
  }
}

/** The preview the server put on an approval (`metadata.preview`), checked. */
export function readPreview(value: unknown): ApprovalPreview | null {
  const raw = record(value)
  if (!raw) return null
  if (raw.kind === 'create') {
    const preview = task(raw.task, false)
    return preview ? { kind: 'create', task: { ...preview, href: null, key: null } } : null
  }
  if (raw.kind === 'change') {
    const current = task(raw.task, false)
    if (!current) return null
    const changes = list(raw.changes)
      .map(record)
      .flatMap((entry) => (entry ? [{ field: str(entry.field, 40), before: str(entry.before), after: str(entry.after) }] : []))
      .filter((entry) => entry.field)
    return { kind: 'change', task: current, changes }
  }
  return null
}

/** Groups of a task list: by project when there is more than one, else by status; with their exact counts. */
export function groupTasks(items: TaskCardData[]): { label: string; tasks: TaskCardData[] }[] {
  const projects = new Set(items.map((item) => item.project?.key ?? ''))
  const byProject = projects.size > 1
  const groups = new Map<string, TaskCardData[]>()
  for (const item of items) {
    const label = byProject ? (item.project?.name || 'No project') : (item.status?.name ?? 'No status')
    groups.set(label, [...(groups.get(label) ?? []), item])
  }
  return [...groups].map(([label, tasks]) => ({ label, tasks }))
}

/** What the panel knows of a task key: only keys that came in card data of this conversation link anywhere. */
export type KnownTask = { key: string; title: string; href: string; status: CardStatus | null }

type CallPart = { type: string; toolName?: string; result?: unknown }

/** Every task in the cards of the conversation's results, by key (the latest card of a key wins). */
export function knownTasks(messages: readonly { role: string; content: readonly CallPart[] }[]): Map<string, KnownTask> {
  const known = new Map<string, KnownTask>()
  const add = (item: TaskCardData) => {
    if (item.key && item.href) known.set(item.key, { key: item.key, title: item.title, href: item.href, status: item.status })
  }
  for (const message of messages) {
    if (message.role !== 'assistant') continue
    for (const part of message.content) {
      if (part.type !== 'tool-call' || part.result === undefined) continue
      const card = readCard(parseResult(part.result))
      if (card?.kind === 'task') add(card)
      else if (card?.kind === 'tasks') card.items.forEach(add)
    }
  }
  return known
}

function parseResult(result: unknown): unknown {
  if (typeof result !== 'string') return result
  try {
    return JSON.parse(result) as unknown
  } catch {
    return null
  }
}

export type KeyMatch = { start: number; end: number; task: KnownTask }

/** The known task keys in a run of plain text, as whole words (`GEN-1` but not `XGEN-1`, `GEN-12` or `GEN-1a`). */
export function findKnownKeys(text: string, known: ReadonlyMap<string, KnownTask>): KeyMatch[] {
  if (known.size === 0) return []
  const matches: KeyMatch[] = []
  for (const match of text.matchAll(/(?<![A-Za-z0-9_-])([A-Za-z0-9_]{1,20}-[1-9]\d{0,14})(?![A-Za-z0-9_]|-\d)/g)) {
    const task = known.get(match[1])
    if (task) matches.push({ start: match.index, end: match.index + match[1].length, task })
  }
  return matches
}
