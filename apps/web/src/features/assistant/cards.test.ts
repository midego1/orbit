import { expect, test } from 'bun:test'
import { cardPath, findKnownKeys, groupTasks, knownTasks, readCard, readPreview, type KnownTask, type TaskCardData } from './cards'

const W = '0193aaaa-bbbb-7ccc-8ddd-eeeeeeeeeeee'

function task(id: string, key: string, extra: Record<string, unknown> = {}) {
  return {
    kind: 'task',
    id,
    key,
    title: `Task ${key}`,
    href: `/tasks/${id}?workspace=${W}`,
    status: { name: 'Todo', category: 'unstarted', color: '#94a3b8' },
    priority: 'high',
    assignees: [{ name: 'Ada' }],
    due: '2026-11-30',
    project: { name: 'Launch', key: 'LCH' },
    ...extra,
  }
}

test('card links are in-app task, doc or project paths only', () => {
  expect(cardPath(`/tasks/t1?workspace=${W}`)).toBe(`/tasks/t1?workspace=${W}`)
  expect(cardPath(`/docs/d-1?workspace=${W}`)).toBe(`/docs/d-1?workspace=${W}`)
  expect(cardPath(`/tasks?project=p1&workspace=${W}`)).toBe(`/tasks?project=p1&workspace=${W}`)
  for (const bad of [
    '//evil.example/tasks/t1?workspace=w',
    'https://evil.example/tasks/t1?workspace=w',
    'javascript:alert(1)',
    '/settings/ai',
    '/tasks/../admin?workspace=w',
    '/tasks/t1?workspace=w&next=//evil',
    `/tasks/t1?workspace=${W}#x`,
    '',
    42,
    null,
  ]) {
    expect(cardPath(bad)).toBeNull()
  }
})

test('a task card keeps user text as plain strings and drops bad fields', () => {
  const card = readCard({
    card: task('t1', 'LCH-1', {
      title: '<img src=x onerror=alert(1)>',
      status: { name: '**bold**', category: 'evil', color: 'red;background:url(x)' },
      priority: 'extreme',
      due: 'tomorrow',
      assignees: [{ name: '<script>' }, 'x', { name: 5 }],
    }),
  }, 'get_task')
  expect(card?.kind).toBe('task')
  const t = card as TaskCardData
  expect(t.title).toBe('<img src=x onerror=alert(1)>')
  expect(t.status).toEqual({ name: '**bold**', category: 'unstarted', color: null })
  expect(t.priority).toBe('none')
  expect(t.due).toBeNull()
  expect(t.assignees).toEqual([{ name: '<script>' }])
})

test('a task without a valid link gives no card, and a list drops such rows', () => {
  expect(readCard({ card: task('t1', 'LCH-1', { href: 'https://evil.example' }) }, 'get_task')).toBeNull()
  const list = readCard({
    card: { kind: 'tasks', totals: { tasks: 2, open: 2, done: 0, byProject: [], byStatus: [], complete: true }, items: [task('t1', 'LCH-1'), task('t2', 'LCH-2', { href: '//x' })] },
  }, 'list_tasks')
  expect(list?.kind === 'tasks' && list.items.map((item) => item.key)).toEqual(['LCH-1'])
  expect(list?.kind === 'tasks' && list.totals.tasks).toBe(2)
})

test('a list is grouped by project when it spans projects, else by status', () => {
  const a = readCard({ card: task('t1', 'LCH-1') }, 'get_task') as TaskCardData
  const b = { ...a, key: 'GEN-1', project: { name: 'General', key: 'GEN' } }
  const c = { ...a, key: 'LCH-2', status: { name: 'Done', category: 'completed' as const, color: null } }
  expect(groupTasks([a, b, c]).map((group) => [group.label, group.tasks.length])).toEqual([
    ['Launch', 2],
    ['General', 1],
  ])
  expect(groupTasks([a, c]).map((group) => [group.label, group.tasks.length])).toEqual([
    ['Todo', 1],
    ['Done', 1],
  ])
})

test('an approval preview is read from the server metadata only in its known shapes', () => {
  const change = readPreview({ kind: 'change', task: task('t7', 'LCH-7'), changes: [{ field: 'Status', before: 'Todo', after: 'In Progress' }] })
  expect(change).toMatchObject({ kind: 'change', changes: [{ field: 'Status', before: 'Todo', after: 'In Progress' }] })
  const create = readPreview({ kind: 'create', task: { ...task('x', 'LCH-9'), href: `/tasks/x?workspace=${W}` } })
  // a task that does not exist yet has no key and no link, whatever the metadata says
  expect(create?.task.href).toBeNull()
  expect(create?.task.key).toBeNull()
  expect(readPreview({ kind: 'delete' })).toBeNull()
  expect(readPreview('nope')).toBeNull()
})

test('known keys come from the cards of tool results only', () => {
  const messages = [
    { role: 'user', content: [{ type: 'text' }] },
    {
      role: 'assistant',
      content: [
        { type: 'tool-call', toolName: 'list_tasks', result: JSON.stringify({ card: { kind: 'tasks', totals: {}, items: [task('t1', 'GEN-1'), task('t4', 'LCH-4')] } }) },
        // a key only in the call's arguments or the model's text is not known
        { type: 'tool-call', toolName: 'get_task', result: JSON.stringify({ identifier: 'WEB-9' }) },
        { type: 'text', text: 'See WEB-9' },
      ],
    },
  ]
  const known = knownTasks(messages)
  expect([...known.keys()].sort()).toEqual(['GEN-1', 'LCH-4'])
})

test('known keys match as whole words only', () => {
  const t: KnownTask = { key: 'GEN-1', title: 'One', href: `/tasks/t1?workspace=${W}`, status: null }
  const known = new Map([['GEN-1', t]])
  const found = (text: string) => findKnownKeys(text, known).map((match) => text.slice(match.start, match.end))
  expect(found('Start GEN-1, then GEN-12 and XGEN-1.')).toEqual(['GEN-1'])
  expect(found('(GEN-1) GEN-1a GEN-1-2 gen-1')).toEqual(['GEN-1'])
  expect(found('nothing here')).toEqual([])
  expect(findKnownKeys('GEN-1', new Map())).toEqual([])
})

test('a card counts only on the step of a tool that has that kind of card', () => {
  const card = { card: task('t1', 'LCH-1') }
  expect(readCard(card, 'get_task')?.kind).toBe('task')
  expect(readCard(card, 'list_labels')).toBeNull()
  expect(readCard(card, undefined)).toBeNull()
  // a summary only from the server's own summary step
  const summary = { card: { kind: 'summary', changes: [{ verb: 'Updated', key: 'LCH-1', title: 'x', href: null }] } }
  expect(readCard(summary, 'orbit_summary')?.kind).toBe('summary')
  expect(readCard(summary, 'list_tasks')).toBeNull()
  // nor do keys from such a card become links
  const messages = [{ role: 'assistant', content: [{ type: 'tool-call', toolName: 'list_labels', result: JSON.stringify(card) }] }]
  expect(knownTasks(messages).size).toBe(0)
})

test('bidi overrides are stripped from card text', () => {
  const card = readCard({ card: task('t1', 'LCH-1', { title: 'Pay \u202Efdp.exe\u2066' }) }, 'get_task') as TaskCardData
  expect(card.title).toBe('Pay fdp.exe')
})

test('group counts come from the totals of every matching task when they are complete', () => {
  const a = readCard({ card: task('t1', 'LCH-1') }, 'get_task') as TaskCardData
  const b = { ...a, key: 'GEN-1', project: { name: 'General', key: 'GEN' } }
  const totals = { tasks: 40, open: 30, done: 10, complete: true, shown: 2, byStatus: [], byProject: [{ name: 'Launch', key: 'LCH', count: 25 }, { name: 'General', key: 'GEN', count: 15 }] }
  expect(groupTasks([a, b], totals).map((group) => [group.label, group.count])).toEqual([
    ['Launch', 25],
    ['General', 15],
  ])
  expect(groupTasks([a, b], { ...totals, complete: false }).map((group) => group.count)).toEqual([1, 1])
})
