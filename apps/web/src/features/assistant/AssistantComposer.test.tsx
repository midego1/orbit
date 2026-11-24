import { afterEach, expect, mock, test } from 'bun:test'
import { useRef, useState } from 'react'
import { act, fireEvent, waitFor } from '@testing-library/react'
import { renderWithWorkspace } from '@/test/renderWithWorkspace'
import { AssistantComposer, type ChatModels } from './AssistantComposer'
import type { Mention } from './composer'

const HOSTILE = '<img src=x onerror="alert(1)">'
const originalFetch = globalThis.fetch
afterEach(() => {
  globalThis.fetch = originalFetch
})

function serve() {
  const task = (id: string, number: number, title: string) => ({
    id, number, title, workspace_id: 'w1', project_id: 'p1', status_id: 's1', description: '', position: 0, priority: 'none',
    assignee_ids: [], creator_id: 'u1', label_ids: [], created_at: '2026-11-01T00:00:00Z', updated_at: '2026-11-01T00:00:00Z', version: 1,
  })
  globalThis.fetch = (async (input: RequestInfo | URL) => {
    const url = new URL(String(input instanceof Request ? input.url : input), 'http://orbit.test')
    const path = url.pathname
    if (path.endsWith('/statuses')) return Response.json({ items: [], next_cursor: null })
    if (path.endsWith('/projects')) return Response.json({ items: [{ id: 'p1', key: 'ENG', name: 'Engineering', color: '#888', version: 1 }], next_cursor: null })
    if (path.endsWith('/members')) {
      return Response.json({
        items: [{ id: 'm1', user_id: 'u1', email: 'ada@orbit.test', display_name: 'Ada', avatar_url: null, role: 'member', version: 1, can_change_role: false, can_remove: false, can_transfer_ownership: false }],
        next_cursor: null,
      })
    }
    if (path.endsWith('/pages/recent')) return Response.json({ items: [{ id: 'd1', title: 'Roadmap', icon: null, kind: 'page', parent_id: null, private: false, teamspace_id: null, database_title: null }] })
    if (path.endsWith('/pages/search')) return Response.json({ items: [] })
    if (path.endsWith('/tasks')) return Response.json({ items: [task('t1', 1, HOSTILE), task('t12', 12, 'Twelve')], next_cursor: null })
    return Response.json({ items: [], next_cursor: null })
  }) as unknown as typeof fetch
}

function Harness({ onSend, onNewChat, running = false, chatModels }: { onSend: (text: string, mentions: Mention[]) => boolean; onNewChat: () => void; running?: boolean; chatModels?: ChatModels }) {
  const [draft, setDraft] = useState('')
  const [mentions, setMentions] = useState<Mention[]>([])
  const input = useRef<HTMLTextAreaElement>(null)
  return (
    <AssistantComposer
      workspaceId="w1"
      draft={draft}
      setDraft={setDraft}
      mentions={mentions}
      setMentions={setMentions}
      inputRef={input}
      running={false}
      waiting={false}
      loading={false}
      onSend={onSend}
      onStop={() => {}}
      onNewChat={onNewChat}
      newChatDisabled={running}
      chatModels={chatModels}
    />
  )
}

function setup(path = '/tasks/ENG-1', running = false, chatModels?: ChatModels) {
  serve()
  const onSend = mock((_text: string, _mentions: Mention[]) => true)
  const onNewChat = mock(() => {})
  const view = renderWithWorkspace(<Harness onSend={onSend} onNewChat={onNewChat} running={running} chatModels={chatModels} />, { path })
  const field = view.getByRole('combobox', { name: 'Message Ask AI' }) as HTMLTextAreaElement
  const type = (text: string) => fireEvent.change(field, { target: { value: text, selectionStart: text.length } })
  const key = (name: string) => fireEvent.keyDown(field, { key: name })
  const selected = () => view.getAllByRole('option').find((option) => option.getAttribute('aria-selected') === 'true')?.textContent
  return { view, field, type, key, selected, onSend, onNewChat }
}

test('"/" opens the command menu; arrows move, Enter writes the prompt, Escape closes', async () => {
  const { view, field, type, key, selected } = setup()
  type('/')
  expect(view.getByRole('listbox', { name: 'Commands' })).toBeTruthy()
  expect(view.getAllByRole('option').map((option) => option.textContent)).toEqual([
    '/summarizeSummarize this page',
    '/planPlan my week',
    '/standupWhat changed since yesterday',
    '/newStart a new chat',
  ])
  expect(field.getAttribute('aria-expanded')).toBe('true')
  expect(selected()).toContain('/summarize')
  key('ArrowUp')
  expect(selected()).toContain('/new')
  key('ArrowDown')
  key('ArrowDown')
  expect(selected()).toContain('/plan')
  expect(field.getAttribute('aria-activedescendant')).toBe(view.getAllByRole('option')[1]!.id)
  key('Enter')
  expect(field.value).toBe('Help me plan this week: which open tasks assigned to me come first, what is overdue, and what can wait?')
  expect(view.queryByRole('listbox')).toBeNull()

  type('/st')
  expect(view.getAllByRole('option')).toHaveLength(1)
  key('Escape')
  expect(view.queryByRole('listbox')).toBeNull()
  // typing on opens it again
  type('/sta')
  expect(view.getByRole('listbox')).toBeTruthy()
})

test('/summarize fits the page, and a typed "/summarize …" is sent as its prompt', () => {
  const { field, type, key, onSend } = setup('/docs/abc')
  type('/summ')
  key('Tab')
  expect(field.value).toBe('Summarize this doc in a few short points.')
  type('/summarize in Dutch')
  key('Enter')
  expect(onSend).toHaveBeenCalledWith('Summarize this doc in a few short points. in Dutch', [])
})

test('/new starts a new chat and sends nothing', () => {
  const { type, key, onSend, onNewChat } = setup()
  type('/new')
  key('Enter')
  expect(onNewChat).toHaveBeenCalledTimes(1)
  expect(onSend).not.toHaveBeenCalled()
})

test('"@" lists people, tasks, projects and docs; a pick goes in as a chip and is sent by id', async () => {
  const { view, field, type, key, onSend } = setup()
  type('Look at @')
  await waitFor(() => expect(view.getByRole('listbox', { name: 'Mention a task, project, doc or person' })).toBeTruthy())
  await waitFor(() => expect(view.getAllByRole('option').length).toBe(5))
  const rows = view.getAllByRole('option').map((option) => option.textContent)
  expect(rows[0]).toContain('Ada')
  // a hostile title is shown as text, never as markup
  expect(rows[1]).toContain(HOSTILE)
  expect(view.container.querySelector('img[src="x"]')).toBeNull()
  expect(rows[2]).toContain('Twelve')
  expect(rows[3]).toContain('Engineering')
  expect(rows[4]).toContain('Roadmap')

  key('ArrowDown')
  key('Enter')
  expect(field.value).toBe('Look at @ENG-1 ')
  const chips = view.getByRole('list', { name: 'Mentioned' })
  expect(chips.textContent).toContain('ENG-1')

  type('Look at @ENG-1 and @Road')
  await waitFor(() => expect(view.getAllByRole('option').some((option) => option.textContent?.includes('Roadmap'))).toBe(true))
  const roadmap = view.getAllByRole('option').findIndex((option) => option.textContent?.includes('Roadmap'))
  act(() => {
    fireEvent.mouseDown(view.getAllByRole('option')[roadmap]!)
  })
  expect(field.value).toBe('Look at @ENG-1 and @Roadmap ')
  key('Enter')
  expect(onSend).toHaveBeenCalledWith('Look at @ENG-1 and @Roadmap', [
    { kind: 'task', id: 't1', label: 'ENG-1' },
    { kind: 'doc', id: 'd1', label: 'Roadmap' },
  ])
})

test('a mention whose text was deleted is not sent; the chip removes its text', async () => {
  const { view, field, type, key, onSend } = setup()
  type('@')
  await waitFor(() => expect(view.getAllByRole('option').length).toBe(5))
  key('Enter')
  expect(field.value).toBe('@Ada ')
  fireEvent.click(view.getByRole('button', { name: 'Remove Ada' }))
  expect(field.value).toBe('')
  type('@')
  await waitFor(() => expect(view.getAllByRole('option').length).toBe(5))
  key('Enter')
  type('hello')
  key('Enter')
  expect(onSend).toHaveBeenCalledWith('hello', [])
})

test('removing a chip takes out only its own mention, not a longer one that starts the same', async () => {
  const { view, field, type, key, onSend } = setup()
  type('@')
  await waitFor(() => expect(view.getAllByRole('option').length).toBe(5))
  key('ArrowDown')
  key('Enter')
  type('@ENG-1 @')
  await waitFor(() => expect(view.getAllByRole('option').length).toBe(5))
  key('ArrowDown')
  key('ArrowDown')
  key('Enter')
  expect(field.value).toBe('@ENG-1 @ENG-12 ')
  fireEvent.click(view.getByRole('button', { name: 'Remove ENG-1' }))
  expect(field.value).toBe('@ENG-12 ')
  type('@ENG-12 next')
  key('Enter')
  expect(onSend).toHaveBeenCalledWith('@ENG-12 next', [{ kind: 'task', id: 't12', label: 'ENG-12' }])
})

test('/new while a run goes on keeps the draft and starts nothing', () => {
  const { field, type, key, onNewChat } = setup('/tasks', true)
  type('/new')
  key('Enter')
  expect(onNewChat).not.toHaveBeenCalled()
  expect(field.value).toBe('/new')
})

test('with no rows yet, Enter waits for the search but Tab moves on', () => {
  const { view, field, type, onSend } = setup()
  type('@zz')
  expect(view.getByRole('listbox')).toBeTruthy()
  expect(view.queryAllByRole('option')).toHaveLength(0)
  // fireEvent returns false when the handler prevented the default
  expect(fireEvent.keyDown(field, { key: 'Enter' })).toBe(false)
  expect(fireEvent.keyDown(field, { key: 'Tab' })).toBe(true)
  expect(onSend).not.toHaveBeenCalled()
})

test('Enter while an input method composes neither sends nor picks (Safari reports keyCode 229)', async () => {
  const { view, field, type, onSend } = setup()
  type('hello')
  expect(fireEvent.keyDown(field, { key: 'Enter', isComposing: true })).toBe(true)
  expect(fireEvent.keyDown(field, { key: 'Enter', keyCode: 229 })).toBe(true)
  expect(onSend).not.toHaveBeenCalled()
  type('@')
  await waitFor(() => expect(view.getAllByRole('option').length).toBe(5))
  fireEvent.keyDown(field, { key: 'Enter', keyCode: 229 })
  expect(field.value).toBe('@')
  fireEvent.keyDown(field, { key: 'Escape' })
  type('hello')
  fireEvent.keyDown(field, { key: 'Enter' })
  expect(onSend).toHaveBeenCalledWith('hello', [])
})

test('/model shows only when the workspace allows more than one model, and lists only the allowed ones', async () => {
  const single = setup('/tasks', false, { model: 'claude-sonnet-5-5', models: ['claude-sonnet-5-5'], onPick: () => {} })
  single.type('/m')
  expect(single.view.queryByRole('listbox', { name: 'Commands' })).toBeNull()
  single.view.unmount()

  const onPick = mock((_model: string) => {})
  const { view, field, type, key, selected, onSend } = setup('/tasks', false, { model: 'claude-sonnet-5-5', models: ['claude-sonnet-5-5', 'claude-haiku-5-5'], onPick })
  type('/m')
  expect(selected()).toBe('/modelPick the model of this chat')
  key('Enter')
  await waitFor(() => expect(field.value).toBe('/model '))
  // the allowed models only: Opus is not offered here
  const models = view.getByRole('listbox', { name: 'Models' })
  expect(models.textContent).toContain('Sonnet 5.5')
  expect(models.textContent).toContain('Haiku 5.5')
  expect(models.textContent).not.toContain('Opus')
  type('/model hai')
  expect(view.getAllByRole('option').map((option) => option.textContent)).toEqual(['Haiku 5.5'])
  key('Enter')
  expect(onPick).toHaveBeenCalledWith('claude-haiku-5-5')
  await waitFor(() => expect(field.value).toBe(''))
  // a model that is not allowed is never picked and never sent to the model as a message
  type('/model opus')
  expect(view.queryByRole('listbox', { name: 'Models' })).toBeNull()
  key('Enter')
  expect(onPick).toHaveBeenCalledTimes(1)
  expect(onSend).not.toHaveBeenCalled()
  await act(async () => {})
})
