import { expect, test } from 'bun:test'
import { fireEvent, render } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { BatchApprovalCard, type BatchDecision } from './BatchApprovalCard'
import { detailsOpen, readBatch, type BatchItem } from './cards'

const W = '0193aaaa-bbbb-7ccc-8ddd-eeeeeeeeeeee'

function preview(n: number) {
  return {
    kind: 'create',
    task: { kind: 'task', id: null, key: null, title: `Task ${n}`, href: null, status: { name: 'Todo', category: 'unstarted', color: '#94a3b8' }, priority: 'none', assignees: [], due: null, project: { name: 'Launch', key: 'LCH' } },
  }
}

/** Eight changes as the server sends them on the interrupt: six creates and two updates, one with a description. */
function batch(): BatchItem[] {
  const items: unknown[] = Array.from({ length: 6 }, (_, i) => ({
    id: `c${i + 1}`,
    tool: 'create_task',
    risk: 'write',
    freeText: false,
    args: { project_id: 'p1', status_id: 's1', title: `Task ${i + 1}` },
    preview: preview(i + 1),
  }))
  items.push({
    id: 'c7',
    tool: 'update_task',
    risk: 'dangerous',
    freeText: true,
    args: { task_id: 't7', expected_version: 3, description: '<img src=x onerror=alert(1)> **bold** ```js\nfence\n```' },
    preview: { kind: 'change', task: { kind: 'task', key: 'LCH-7', title: 'Seven', href: `/tasks/t7?workspace=${W}`, priority: 'high', assignees: [], due: null }, changes: [{ field: 'Description', before: 'old', after: 'new' }] },
  })
  items.push({ id: 'c8', tool: 'update_task', risk: 'dangerous', freeText: true, args: { task_id: 't8', expected_version: 1, priority: 'urgent' }, preview: null })
  return readBatch({ items }) ?? []
}

function show(onDecide: (decision: BatchDecision) => void = () => {}) {
  return render(
    <MemoryRouter>
      <BatchApprovalCard items={batch()} onDecide={onDecide} />
    </MemoryRouter>,
  )
}

const boxes = (view: ReturnType<typeof show>) => [...view.container.querySelectorAll('[data-batch-item] [role="checkbox"]')]
const approve = (view: ReturnType<typeof show>) => view.container.querySelector('[data-assistant-approve]') as HTMLButtonElement

test('one group lists every change as its own card, all ticked at first', () => {
  const view = show()
  expect(view.getByText('Ask AI wants to make 8 changes')).toBeTruthy()
  expect(view.container.querySelectorAll('[data-batch-item]').length).toBe(8)
  expect(boxes(view).every((box) => box.getAttribute('aria-checked') === 'true')).toBe(true)
  expect(view.getByText('8 of 8 selected')).toBeTruthy()
  expect(approve(view).textContent).toBe('Approve 8 selected')
  // the server's previews, not the model's text
  expect(view.container.querySelector('[data-batch-item="c3"] [data-slot="task-card"]')?.textContent).toContain('Task 3')
  expect(view.getAllByText('Changes data').length).toBe(2)
})

test('unticked changes are left out of the approval, which names ids only', () => {
  let decision: BatchDecision | null = null
  const view = show((value) => (decision = value))
  fireEvent.click(boxes(view)[1])
  fireEvent.click(boxes(view)[6])
  expect(view.getByText('6 of 8 selected')).toBeTruthy()
  expect(approve(view).textContent).toBe('Approve 6 selected')
  fireEvent.click(approve(view))
  expect(decision as unknown).toEqual({ approved: true, items: ['c1', 'c3', 'c4', 'c5', 'c6', 'c8'] })
  // answered once: the buttons lock
  expect(approve(view).disabled).toBe(true)
})

test('none selected: approve is off; decline all declines the whole batch', () => {
  let decision: BatchDecision | null = null
  const view = show((value) => (decision = value))
  const all = view.getByRole('checkbox', { name: 'Select none' })
  fireEvent.click(all)
  expect(view.getByText('0 of 8 selected')).toBeTruthy()
  expect(approve(view).disabled).toBe(true)
  fireEvent.click(view.getByRole('checkbox', { name: 'Select all' }))
  expect(view.getByText('8 of 8 selected')).toBeTruthy()
  fireEvent.click(view.getByRole('button', { name: 'Decline all' }))
  expect(decision as unknown).toEqual({ approved: false })
})

test('a checkbox toggles from the keyboard and has a name per change', () => {
  const view = show()
  const box = view.getByRole('checkbox', { name: 'Include change 2: Create task: Task 2' })
  box.focus()
  expect(document.activeElement).toBe(box)
  fireEvent.keyDown(box, { key: ' ', code: 'Space' })
  fireEvent.keyUp(box, { key: ' ', code: 'Space' })
  expect(box.getAttribute('aria-checked')).toBe('false')
  expect(view.getByText('7 of 8 selected')).toBeTruthy()
  // Enter does not toggle a checkbox (it must not approve or change anything by accident)
  fireEvent.keyDown(box, { key: 'Enter', code: 'Enter' })
  expect(box.getAttribute('aria-checked')).toBe('false')
})

test('free text is shown in full under Details, as plain text', () => {
  const view = show()
  const item = view.container.querySelector('[data-batch-item="c7"]') as HTMLElement
  expect(item.textContent).toContain('Has text')
  const details = item.querySelector('details') as HTMLDetailsElement
  expect(details.textContent).toContain('<img src=x onerror=alert(1)> **bold** ```js\nfence\n```')
  expect(item.querySelector('img')).toBeNull()
  expect(item.querySelector('strong')).toBeNull()
  // a change without a preview shows its arguments at once
  const plain = view.container.querySelector('[data-batch-item="c8"]') as HTMLElement
  expect(plain.textContent).toContain('urgent')
})

test('a batch from the server is checked: bad ids and tools are dropped', () => {
  const items = readBatch({
    items: [
      { id: 'c1', tool: 'create_task', risk: 'write', args: {} },
      { id: '../x', tool: 'create_task', args: {} },
      { id: 'c3', tool: 'create task<script>', args: {} },
      { id: 'c4', tool: 'update_task', risk: 'read', args: [] },
    ],
  })
  expect(items?.map((item) => item.id)).toEqual(['c1', 'c4'])
  // an unknown risk is never shown as a read; free text is assumed unless the server says otherwise
  expect(items?.[1].risk).toBe('write')
  expect(items?.[1].freeText).toBe(true)
  expect(readBatch({ items: [] })).toBeNull()
  expect(readBatch(null)).toBeNull()
})

test('Details open at once for rich text or arguments the preview does not show, and stay folded otherwise', () => {
  const view = show()
  const open = (id: string) => (view.container.querySelector(`[data-batch-item="${id}"] details`) as HTMLDetailsElement | null)?.open
  // c7 writes a description: open
  expect(open('c7')).toBe(true)
  // c1: a create whose preview shows every argument: folded
  expect(open('c1')).toBe(false)
  expect(detailsOpen('create_task', { project_id: 'p', status_id: 's', title: 'T' }, true)).toBe(false)
  // an argument the preview card does not show, such as a parent or a recurrence
  expect(detailsOpen('create_task', { project_id: 'p', status_id: 's', title: 'T', parent_task_id: 'GEN-1' }, true)).toBe(true)
  expect(detailsOpen('update_task', { task_id: 't', expected_version: 1, recurrence: null }, true)).toBe(true)
  expect(detailsOpen('update_task', { task_id: 't', expected_version: 1, status_id: 's' }, true)).toBe(false)
  // rich text of any change, preview or not; an empty description is not text
  expect(detailsOpen('create_comment', { task_id: 't', body: 'hi' }, false)).toBe(true)
  expect(detailsOpen('create_task', { project_id: 'p', status_id: 's', title: 'T', description: '' }, true)).toBe(false)
})

test('an answer that did not go through lets the batch be answered again', async () => {
  let calls = 0
  const view = render(
    <MemoryRouter>
      <BatchApprovalCard
        items={batch()}
        onDecide={async () => {
          calls += 1
          throw new Error('offline')
        }}
      />
    </MemoryRouter>,
  )
  fireEvent.click(approve(view as never))
  await new Promise((resolve) => setTimeout(resolve, 0))
  expect(calls).toBe(1)
  expect(approve(view as never).disabled).toBe(false)
})

test('a change without a preview opens its Details when it names something by id', () => {
  const view = show()
  // c8: an update without a preview; which task it changes is an id, shown under Details
  expect((view.container.querySelector('[data-batch-item="c8"] details') as HTMLDetailsElement).open).toBe(true)
  expect(detailsOpen('create_task_relation', { task_id: 't', other_task_id: 'o', type: 'blocks' }, false)).toBe(true)
  expect(detailsOpen('update_label', { label_ids: ['l1'] }, false)).toBe(true)
  // a name alone, or only the workspace: nothing hidden that matters
  expect(detailsOpen('update_project', { name: 'Launch' }, false)).toBe(false)
  expect(detailsOpen('create_project', { workspace_id: 'w', name: 'Launch' }, false)).toBe(false)
})
