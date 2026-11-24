import { expect, test } from 'bun:test'
import { act, fireEvent, render, waitFor } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { renderMarkdownBlocks } from '@/lib/markdown'
import { MarkdownTextContext } from '@/lib/markdownTextContext'
import { PreviewCard, ResultCard } from './AssistantCards'
import { taskChipRun } from './taskChipRun'
import { UndoContext, type UndoChanges } from './undoContext'
import { readCard, readPreview, totalsLine, type CardData, type KnownTask } from './cards'

const W = '0193aaaa-bbbb-7ccc-8ddd-eeeeeeeeeeee'

function task(n: number, project = 'LCH', status = 'Todo', category = 'unstarted') {
  return {
    kind: 'task',
    key: `${project}-${n}`,
    title: `Task ${n}`,
    href: `/tasks/${project.toLowerCase()}${n}?workspace=${W}`,
    status: { name: status, category, color: '#94a3b8' },
    priority: 'medium',
    assignees: [],
    due: null,
    project: { name: project === 'LCH' ? 'Launch' : 'General', key: project },
  }
}

function show(node: React.ReactNode) {
  return render(<MemoryRouter>{node}</MemoryRouter>)
}

test('a task card links to the task and shows its title as text, not HTML', () => {
  const card = readCard({ card: { ...task(1), title: '<b>bold</b> <img src=x onerror=alert(1)>' } }, 'get_task') as CardData
  const view = show(<ResultCard card={card} />)
  const link = view.container.querySelector('a[data-slot="task-card"]')
  expect(link?.getAttribute('href')).toBe(`/tasks/lch1?workspace=${W}`)
  expect(link?.textContent).toContain('<b>bold</b> <img src=x onerror=alert(1)>')
  expect(view.container.querySelector('img[src="x"]')).toBeNull()
  expect(view.container.querySelector('b')).toBeNull()
  expect(link?.textContent).toContain('LCH-1')
  expect(link?.textContent).toContain('Medium')
})

test('a long task list shows five rows with exact totals until "Show all"', () => {
  const items = [...Array.from({ length: 6 }, (_, i) => task(i + 1)), task(1, 'GEN', 'Done', 'completed'), task(2, 'GEN')]
  const card = readCard({
    card: {
      kind: 'tasks',
      totals: { tasks: 8, open: 7, done: 1, byProject: [], byStatus: [], complete: true, shown: 8 },
      items,
    },
  }, 'list_tasks') as CardData
  const view = show(<ResultCard card={card} />)
  expect(view.getByText(/8 tasks · 7 open · 1 done/)).toBeTruthy()
  expect(view.container.querySelectorAll('[data-slot="task-row"]').length).toBe(5)
  // the group counts are the whole group's, not the rows shown
  expect(view.getByText('Launch').closest('p')?.textContent).toBe('Launch6')
  fireEvent.click(view.getByRole('button', { name: 'Show all 8' }))
  expect(view.container.querySelectorAll('[data-slot="task-row"]').length).toBe(8)
  expect(view.getByText('General').closest('p')?.textContent).toBe('General2')
})

test('a short list shows every row and no "Show all"', () => {
  const card = readCard({ card: { kind: 'tasks', totals: { tasks: 2, open: 2, done: 0, complete: false, shown: 2 }, items: [task(1), task(2)] } }, 'list_tasks') as CardData
  const view = show(<ResultCard card={card} />)
  expect(view.container.querySelectorAll('[data-slot="task-row"]').length).toBe(2)
  expect(view.queryByRole('button', { name: /Show all/ })).toBeNull()
  expect(view.getByText(/listed tasks only/)).toBeTruthy()
})

test('a change preview strikes the old value through next to the new one', () => {
  const preview = readPreview({ kind: 'change', task: task(7), changes: [{ field: 'Status', before: 'Todo', after: 'In Progress' }] })!
  const view = show(<PreviewCard preview={preview} />)
  expect(view.container.querySelector('del')?.textContent).toBe('was Todo')
  expect(view.container.querySelector('ins')?.textContent).toBe('becomes In Progress')
})

test('a create preview is a task card without a link', () => {
  const preview = readPreview({ kind: 'create', task: { ...task(1), title: 'Write the launch post' } })!
  const view = show(<PreviewCard preview={preview} />)
  expect(view.container.querySelector('a')).toBeNull()
  expect(view.getByText('Write the launch post')).toBeTruthy()
  expect(view.getByText('New')).toBeTruthy()
})

test('a summary lists each change with its link', () => {
  const card = readCard({
    card: { kind: 'summary', changes: [{ verb: 'Updated', key: 'LCH-7', title: 'Seven', href: `/tasks/t7?workspace=${W}` }, { verb: 'Created', key: null, title: 'New', href: 'https://evil.example' }] },
  }, 'orbit_summary') as CardData
  const view = show(<ResultCard card={card} />)
  expect(view.getByText('2 changes applied')).toBeTruthy()
  const links = view.container.querySelectorAll('a')
  expect(links.length).toBe(1)
  expect(links[0].getAttribute('href')).toBe(`/tasks/t7?workspace=${W}`)
})

test('task keys in an answer become chips only when known, never in code', () => {
  const known = new Map<string, KnownTask>([['GEN-1', { key: 'GEN-1', title: 'One <i>x</i>', href: `/tasks/g1?workspace=${W}`, status: null }]])
  const text = 'Start GEN-1 and LCH-4.\n\nInline `GEN-1` stays code.\n\n```\nGEN-1\n```'
  const view = show(<MarkdownTextContext value={taskChipRun(known)}>{renderMarkdownBlocks(text, 't', [], { noExternalImages: true })}</MarkdownTextContext>)
  const chips = view.container.querySelectorAll('a[data-slot="task-chip"]')
  expect(chips.length).toBe(1)
  expect(chips[0].getAttribute('href')).toBe(`/tasks/g1?workspace=${W}`)
  expect(chips[0].textContent).toBe('GEN-1')
  // an unknown key stays text
  expect(view.container.textContent).toContain('LCH-4')
  expect([...view.container.querySelectorAll('a')].some((a) => a.textContent?.includes('LCH-4'))).toBe(false)
})

test('the counts line says how many of the counted tasks are listed, and leaves out unknown open/done', () => {
  const card = (totals: object, items = [task(1), task(2)]) => readCard({ card: { kind: 'tasks', totals, items } }, 'list_tasks')
  const many = card({ tasks: 120, open: 100, done: 20, complete: true, shown: 100 })
  expect(many?.kind === 'tasks' && totalsLine(many)).toBe('120 tasks · 100 open · 20 done · 2 listed')
  const unknown = card({ tasks: 2, open: null, done: null, complete: true, shown: 2 })
  expect(unknown?.kind === 'tasks' && totalsLine(unknown)).toBe('2 tasks')
})

test('a group header says "N of M" when the server counted more tasks than the card lists', () => {
  const card = readCard(
    {
      card: {
        kind: 'tasks',
        totals: { tasks: 40, open: 40, done: 0, complete: true, shown: 3, byStatus: [], byProject: [{ name: 'Launch', key: 'LCH', count: 30 }, { name: 'General', key: 'GEN', count: 10 }] },
        items: [task(1), task(2), task(1, 'GEN')],
      },
    },
    'list_tasks',
  ) as CardData
  const view = show(<ResultCard card={card} />)
  expect(view.getByText('Launch').closest('p')?.textContent).toBe('Launch2 of 30')
  expect(view.getByText('General').closest('p')?.textContent).toBe('General1 of 10')
})

// ---- the summary card: outcomes and undo ----

function summary(changes: unknown[]) {
  return readCard({ card: { kind: 'summary', changes } }, 'orbit_summary') as CardData
}

const applied = (n: number, undo: unknown = { id: `u${n}`, state: 'available' }) => ({
  verb: n % 2 ? 'Created' : 'Updated',
  key: `LCH-${n}`,
  title: `Task ${n}`,
  href: `/tasks/t${n}?workspace=${W}`,
  outcome: 'applied',
  undo,
})

function showUndo(card: CardData, undo: UndoChanges | null) {
  return render(
    <MemoryRouter>
      <UndoContext value={undo}>
        <ResultCard card={card} />
      </UndoContext>
    </MemoryRouter>,
  )
}

test('a batch summary counts applied, failed and skipped changes, with the reason of each', () => {
  const card = summary([
    applied(1),
    applied(2),
    { verb: 'Create task', key: null, title: 'Three', href: null, outcome: 'failed', note: 'status_id must be a status of the project' },
    { verb: 'Create task', key: null, title: 'Four', href: null, outcome: 'skipped', note: 'an earlier change of the batch failed' },
    { verb: 'Update task', key: 'LCH-5', title: 'Five', href: null, outcome: 'skipped', note: 'not selected by the user' },
  ])
  const view = showUndo(card, null)
  expect(view.getByText('2 applied · 1 failed · 2 not run')).toBeTruthy()
  expect(view.getByText('Failed: status_id must be a status of the project')).toBeTruthy()
  expect(view.getByText('Not run: not selected by the user')).toBeTruthy()
  // only applied changes link anywhere; without the panel's undo there are no Undo buttons
  expect(view.container.querySelectorAll('[data-change-outcome="applied"] a').length).toBe(2)
  expect(view.container.querySelectorAll('[data-change-outcome="skipped"] a').length).toBe(0)
  expect(view.queryByRole('button', { name: /Undo/ })).toBeNull()
})

test('Undo asks the server for that change only and shows it as undone', async () => {
  const asked: string[][] = []
  const view = showUndo(summary([applied(1), applied(2)]), async (ids) => {
    asked.push(ids)
    return ids.map((id) => ({ id, outcome: 'undone', message: 'LCH-1 was moved to the trash.' }))
  })
  expect(view.getByText('2 changes applied')).toBeTruthy()
  await act(async () => {
    fireEvent.click(view.getByRole('button', { name: 'Undo: Created LCH-1' }))
  })
  expect(asked).toEqual([['u1']])
  await waitFor(() => expect(view.getByText('Undone')).toBeTruthy())
  // the undone new task is in the trash: no link to it any more
  expect(view.container.querySelectorAll('[data-change-outcome="applied"] a').length).toBe(1)
  // the other change can still be undone; Undo all is gone with one left
  expect(view.getByRole('button', { name: 'Undo: Updated LCH-2' })).toBeTruthy()
  expect(view.container.querySelector('[data-assistant-undo-all]')).toBeNull()
})

test('Undo all sends every change that can still be undone, and shows a conflict as a message', async () => {
  const asked: string[][] = []
  const view = showUndo(summary([applied(1), applied(2), applied(3, { id: 'u3', state: 'undone' }), applied(4, null)]), async (ids) => {
    asked.push(ids)
    return [
      { id: 'u1', outcome: 'undone', message: '' },
      { id: 'u2', outcome: 'conflict', message: 'LCH-2 was changed after Ask AI’s change (by you or someone else), so it was not undone.' },
    ]
  })
  await act(async () => {
    fireEvent.click(view.container.querySelector('[data-assistant-undo-all]') as HTMLElement)
  })
  expect(asked).toEqual([['u1', 'u2']])
  await waitFor(() => expect(view.getByRole('alert').textContent).toContain('LCH-2 was changed after'))
  expect(view.getByText('Can’t undo')).toBeTruthy()
  expect(view.getAllByText('Undone').length).toBe(2)
  // a change without an undo record has no button
  expect(view.queryByRole('button', { name: 'Undo: Updated LCH-4' })).toBeNull()
})

test('an expired undo says so, and a failed request leaves the button to try again', async () => {
  const view = showUndo(summary([applied(1, { id: 'u1', state: 'expired' }), applied(2)]), async () => {
    throw new Error('Ask AI is still working on this conversation; undo when it has finished.')
  })
  expect(view.getByText('Undo expired')).toBeTruthy()
  await act(async () => {
    fireEvent.click(view.getByRole('button', { name: 'Undo: Updated LCH-2' }))
  })
  await waitFor(() => expect(view.getByRole('alert').textContent).toContain('still working'))
  expect(view.getByRole('button', { name: 'Undo: Updated LCH-2' })).toBeTruthy()
})

test('summary entries are checked: a bad undo id or state is not trusted', () => {
  const card = summary([applied(1, { id: '../x', state: 'available' }), applied(2, { id: 'u2', state: 'weird' })]) as Extract<CardData, { kind: 'summary' }>
  expect(card.changes[0].undo).toBeNull()
  expect(card.changes[1].undo).toEqual({ id: 'u2', state: 'expired' })
  // an entry from before batches is an applied change
  const old = summary([{ verb: 'Created', key: 'LCH-1', title: 'One', href: null }]) as Extract<CardData, { kind: 'summary' }>
  expect(old.changes[0].outcome).toBe('applied')
})

test('an undo the server saw cut off shows as "Can’t undo" and does not lock the other changes', () => {
  const view = showUndo(summary([applied(1, { id: 'u1', state: 'undoing' }), applied(2)]), async () => [])
  expect(view.queryByText('Undoing…')).toBeNull()
  expect(view.getByText('Can’t undo')).toBeTruthy()
  expect(view.getByRole('alert').textContent).toContain('cut off')
  const other = view.getByRole('button', { name: 'Undo: Updated LCH-2' }) as HTMLButtonElement
  expect(other.disabled).toBe(false)
})
