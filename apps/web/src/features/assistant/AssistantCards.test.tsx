import { expect, test } from 'bun:test'
import { fireEvent, render } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { renderMarkdownBlocks } from '@/lib/markdown'
import { MarkdownTextContext } from '@/lib/markdownTextContext'
import { PreviewCard, ResultCard, taskChipRun } from './AssistantCards'
import { readCard, readPreview, type CardData, type KnownTask } from './cards'

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
  const card = readCard({ card: { ...task(1), title: '<b>bold</b> <img src=x onerror=alert(1)>' } }) as CardData
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
      totals: { tasks: 8, open: 7, done: 1, byProject: [], byStatus: [], complete: true },
      items,
    },
  }) as CardData
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
  const card = readCard({ card: { kind: 'tasks', totals: { tasks: 2, open: 2, done: 0, complete: false }, items: [task(1), task(2)] } }) as CardData
  const view = show(<ResultCard card={card} />)
  expect(view.container.querySelectorAll('[data-slot="task-row"]').length).toBe(2)
  expect(view.queryByRole('button', { name: /Show all/ })).toBeNull()
  expect(view.getByText(/first page/)).toBeTruthy()
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
  }) as CardData
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
