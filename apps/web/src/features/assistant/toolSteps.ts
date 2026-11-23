// How the panel shows one tool call of Ask AI: a title, a short subject, and for a change the arguments as fields for
// the approval card. The server decides what needs approval (apps/server/src/assistant/registry.rs) and sends the risk
// with its interrupt; the risk worked out here is only the fallback for showing a step.

export type StepRisk = 'read' | 'write' | 'dangerous'

export type StepField = { label: string; value: string }

export type StepView = {
  title: string
  /** The task title, doc name or path the call is about, shown after the title. */
  subject: string
  risk: StepRisk
  /** What the change does, in the user's terms: on the approval card. */
  fields: StepField[]
  /** Bookkeeping (versions, positions) and ids without a known name: folded under "Details". */
  details: StepField[]
}

const STEP_TITLES: Record<string, string> = {
  get_me: 'Checked your account',
  list_projects: 'Looked up projects',
  list_tasks: 'Looked up tasks',
  get_task: 'Read a task',
  list_task_statuses: 'Looked up statuses',
  list_labels: 'Looked up labels',
  list_milestones: 'Looked up milestones',
  list_cycles: 'Looked up cycles',
  list_comments: 'Read comments',
  list_members: 'Looked up members',
  list_docs: 'Looked up docs',
  search_docs: 'Searched docs',
  get_doc: 'Read a doc',
  list_conversations: 'Looked up chats',
  list_messages: 'Read a chat',
  get_thread: 'Read a thread',
  search_messages: 'Searched chat',
  get_database: 'Read a database',
  query_database: 'Read a database',
  list_views: 'Looked up views',
  get_view: 'Read a view',
  list_notifications: 'Looked at the Inbox',
  open_page: 'Opened a page',
  navigate: 'Opened a page',
  load_tools: 'Loaded more tools',
  create_task: 'Create task',
  update_task: 'Update task',
  restore_task: 'Restore task',
  bulk_update_tasks: 'Update several tasks',
  create_comment: 'Comment on task',
  update_comment: 'Edit comment',
  create_task_relation: 'Link tasks',
  create_project: 'Create project',
  update_project: 'Update project',
  create_task_status: 'Create status',
  update_task_status: 'Update status',
  create_label: 'Create label',
  update_label: 'Update label',
  create_milestone: 'Create milestone',
  update_milestone: 'Update milestone',
  create_cycle: 'Create cycle',
  update_cycle: 'Update cycle',
  create_doc: 'Create doc',
  update_doc: 'Replace doc content',
  move_doc: 'Move doc',
  duplicate_doc: 'Duplicate doc',
  restore_doc: 'Restore doc',
  create_doc_comment: 'Comment on doc',
  create_database: 'Create database',
  create_database_row: 'Add database row',
  update_database_row: 'Update database row',
  create_view: 'Create view',
  update_view: 'Update view',
}

const FIELD_NAMES: Record<string, string> = {
  title: 'Title',
  name: 'Name',
  description: 'Description',
  body: 'Text',
  markdown: 'Content',
  project_id: 'Project',
  status_id: 'Status',
  assignee_ids: 'Assignees',
  label_ids: 'Labels',
  priority: 'Priority',
  due_at: 'Due',
  path: 'Page',
}

/** Arguments about how, not what: under Details. */
const BOOKKEEPING = new Set(['workspace_id', 'expected_version', 'version', 'position', 'cursor', 'limit', 'dry_run'])

/** Calls that change something existing in place (MCP marks them destructive): always asked. */
const ALWAYS_ASKED = new Set([
  'update_task',
  'update_comment',
  'update_doc',
  'move_doc',
  'create_task_relation',
  'update_database_row',
  'update_database_view',
  'update_database_property',
  'bulk_update_tasks',
  'move_unfinished_cycle_tasks',
  'mark_all_notifications_read',
])

export function fallbackRisk(tool: string): StepRisk {
  if (ALWAYS_ASKED.has(tool) || /^(delete|trash|purge|empty)_/.test(tool)) return 'dangerous'
  if (/^(create|update|restore|move|duplicate|set|add|remove|reorder|mark)_/.test(tool)) return 'write'
  return 'read'
}

function sentence(tool: string): string {
  const words = tool.replaceAll('_', ' ')
  return words.charAt(0).toUpperCase() + words.slice(1)
}

/** A UUID, a long number, or a list of them: an id the user would not recognise. */
function isMachineId(value: unknown): boolean {
  if (Array.isArray(value)) return value.length > 0 && value.every(isMachineId)
  if (typeof value === 'string') return /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(value) || /^\d{12,}$/.test(value)
  return false
}

function asText(value: unknown): string {
  if (typeof value === 'string') return value
  if (value === null || value === undefined) return ''
  return JSON.stringify(value, null, 2)
}

/** `names` maps project and status ids to their names, so the card says "Project: Website" and not an id. */
export function describeStep(tool: string, args: unknown, names?: ReadonlyMap<string, string>, serverRisk?: unknown): StepView {
  const record = args !== null && typeof args === 'object' && !Array.isArray(args) ? (args as Record<string, unknown>) : {}
  const resolve = (value: unknown): unknown =>
    typeof value === 'string' ? (names?.get(value) ?? value) : Array.isArray(value) ? value.map(resolve) : value
  const fields: StepField[] = []
  const details: StepField[] = []
  for (const [key, raw] of Object.entries(record)) {
    if (raw === undefined || raw === null || raw === '') continue
    const value = resolve(raw)
    const field = { label: FIELD_NAMES[key] ?? sentence(key), value: asText(value) }
    if (BOOKKEEPING.has(key) || isMachineId(value)) details.push(field)
    else fields.push(field)
  }
  const subject = ['title', 'name', 'query', 'path'].map((key) => record[key]).find((value): value is string => typeof value === 'string') ?? ''
  const risk: StepRisk = serverRisk === 'write' || serverRisk === 'dangerous' || serverRisk === 'read' ? serverRisk : fallbackRisk(tool)
  return { title: STEP_TITLES[tool] ?? sentence(tool), subject, risk, fields, details }
}

/** An in-app path for a step's link: a path, or a full link to this same origin. Anything else gives no link. */
export function appPath(url: unknown): string | null {
  if (typeof url !== 'string' || url === '') return null
  if (url.startsWith('/')) return url.startsWith('//') ? null : url
  const origin = globalThis.location?.origin
  if (!origin) return null
  try {
    const parsed = new URL(url)
    return parsed.origin === origin ? `${parsed.pathname}${parsed.search}` : null
  } catch {
    return null
  }
}

/** Tool results arrive as JSON text over AG-UI. */
export function parseToolResult(result: unknown): unknown {
  if (typeof result !== 'string') return result
  try {
    return JSON.parse(result) as unknown
  } catch {
    return result
  }
}

function objectOf(value: unknown): Record<string, unknown> | null {
  return value !== null && typeof value === 'object' && !Array.isArray(value) ? (value as Record<string, unknown>) : null
}

export type StepOutcome = 'running' | 'waiting' | 'done' | 'failed' | 'declined' | 'skipped'

/** What happened to a call, from its approval and its (parsed) result. */
export function stepOutcome(call: {
  approval?: { approved?: boolean; resolution?: string }
  result: unknown
  isError?: boolean
  status: { type: string; reason?: string }
}): StepOutcome {
  const { approval, result, isError, status } = call
  if (approval && approval.approved === undefined && !approval.resolution) return 'waiting'
  if (approval?.approved === false) return 'declined'
  if (approval?.resolution) return 'skipped'
  if (status.type === 'incomplete') return status.reason === 'cancelled' ? 'skipped' : 'failed'
  if (isError) return 'failed'
  if (result === undefined) return 'running'
  const record = objectOf(result)
  if (record?.declined === true) return 'declined'
  if (record?.expired === true || record?.skipped === true) return 'skipped'
  if (typeof record?.error === 'string') return 'failed'
  return 'done'
}

/** A page step the server did not open because Follow along was off: shown as a link, not as done. */
export function notOpened(tool: string, result: unknown): boolean {
  return (tool === 'navigate' || tool === 'open_page') && objectOf(result)?.opened === false
}

/** One short line for a finished call, or '' when there is nothing to add. */
export function resultLine(result: unknown): string {
  if (result === undefined || result === null) return ''
  if (Array.isArray(result)) return result.length === 1 ? '1 result' : `${result.length} results`
  const record = objectOf(result)
  if (record) {
    if (record.declined === true) return 'Declined'
    if (record.expired === true) return 'Expired, not done'
    if (record.skipped === true) return 'Skipped'
    if (typeof record.error === 'string') return clip(record.error, 140)
    for (const key of ['message', 'summary', 'key', 'title', 'name']) {
      const value = record[key]
      if (typeof value === 'string' && value) return clip(value, 80)
    }
    return ''
  }
  return clip(String(result), 80)
}

function clip(text: string, length: number): string {
  return text.length > length ? `${text.slice(0, length)}…` : text
}

type CallPart = { type: string; toolName?: string; toolCallId?: string; result?: unknown }

/** Pages the server's page steps point at (`navigate` after a change or read, and `open_page`), oldest first. Only a
    result the server sent counts, never a call's own arguments. */
export function pageTargets(messages: readonly { role: string; content: readonly CallPart[] }[]): { id: string; path: string }[] {
  const targets: { id: string; path: string }[] = []
  for (const message of messages) {
    if (message.role !== 'assistant') continue
    for (const part of message.content) {
      if (part.type !== 'tool-call' || !part.toolCallId || part.result === undefined) continue
      if (part.toolName !== 'navigate' && part.toolName !== 'open_page') continue
      const path = appPath(objectOf(parseToolResult(part.result))?.url)
      if (path) targets.push({ id: part.toolCallId, path })
    }
  }
  return targets
}
