import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { fireEvent, render, waitFor } from '@testing-library/react'
import { AssistantHistory } from './AssistantHistory'
import type { ConversationSummary } from './api'
import { chatTitle, filterConversations } from './history'

const W = '0193aaaa-bbbb-7ccc-8ddd-eeeeeeeeeeee'
const originalFetch = globalThis.fetch
afterEach(() => {
  globalThis.fetch = originalFetch
})

function chat(id: string, title: string | null, minutesAgo: number, active = false): ConversationSummary {
  const updatedAt = Date.now() - minutesAgo * 60_000
  return { id, title, messageCount: 4, active, createdAt: updatedAt, updatedAt }
}

type Call = { path: string; method: string; body: unknown; query: string }

/** A stand-in server: the history pages, rename and delete. */
function serve(pages: { conversations: ConversationSummary[]; nextCursor: string | null }[]) {
  const calls: Call[] = []
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = new URL(typeof input === 'string' ? input : input instanceof URL ? input.href : input.url, 'http://orbit.test')
    const method = init?.method ?? 'GET'
    const body = typeof init?.body === 'string' ? JSON.parse(init.body) : undefined
    calls.push({ path: url.pathname, method, body, query: url.search })
    if (method === 'GET' && url.pathname.endsWith('/conversations')) {
      const page = url.searchParams.get('before') ? pages[1] : pages[0]
      return Response.json(page)
    }
    if (method === 'PATCH') {
      const id = url.pathname.split('/').at(-1)
      const row = pages.flatMap((page) => page.conversations).find((entry) => entry.id === id)
      return Response.json({ conversation: { ...row, title: (body as { title: string }).title } })
    }
    if (method === 'DELETE') return new Response(null, { status: 204 })
    return Response.json({ code: 'not_found', detail: 'No such conversation.' }, { status: 404 })
  }) as typeof fetch
  return calls
}

function show(props: Partial<Parameters<typeof AssistantHistory>[0]> = {}) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } })
  return render(
    <QueryClientProvider client={client}>
      <AssistantHistory workspaceId={W} onOpen={() => {}} onDeleted={() => {}} {...props} />
    </QueryClientProvider>,
  )
}

test('the history lists the chats newest first, with a title and a relative time', async () => {
  serve([{ conversations: [chat('c1', 'Summarize the open tasks', 5, true), chat('c2', 'What is assigned to me?', 180), chat('c3', null, 60 * 24 * 2)], nextCursor: null }])
  const view = show()
  const list = await view.findByRole('list', { name: 'Earlier chats' })
  const rows = [...list.querySelectorAll('li')].map((row) => row.textContent)
  expect(rows[0]).toContain('Summarize the open tasks')
  expect(rows[0]).toContain('Current chat · 5m')
  expect(rows[1]).toContain('What is assigned to me?')
  expect(rows[1]).toContain('3h')
  expect(rows[2]).toContain('Untitled chat')
  expect(rows[2]).toContain('2d')
  // the open chat is marked for assistive technology too
  expect(view.getByRole('button', { name: /^Summarize the open tasks/ }).getAttribute('aria-current')).toBe('true')
})

test('a title is shown as plain text', async () => {
  serve([{ conversations: [chat('c1', '<img src=x onerror=alert(1)> <b>bold</b>', 1)], nextCursor: null }])
  const view = show()
  expect(await view.findByText('<img src=x onerror=alert(1)> <b>bold</b>')).toBeTruthy()
  expect(view.container.querySelector('img')).toBeNull()
  expect(view.container.querySelector('b')).toBeNull()
})

test('without chats the history says so', async () => {
  serve([{ conversations: [], nextCursor: null }])
  const view = show()
  expect(await view.findByText('No earlier chats yet')).toBeTruthy()
  expect(view.getByText(/show here for 30 days\. Only you can see them\./)).toBeTruthy()
})

test('search filters the loaded titles, by every word in any case', async () => {
  serve([{ conversations: [chat('c1', 'Summarize the open tasks', 5), chat('c2', 'Launch plan', 10)], nextCursor: null }])
  const view = show()
  await view.findByRole('list', { name: 'Earlier chats' })
  fireEvent.change(view.getByRole('searchbox', { name: 'Search chats' }), { target: { value: 'OPEN summ' } })
  expect(view.queryByText('Launch plan')).toBeNull()
  expect(view.getByText('Summarize the open tasks')).toBeTruthy()
  fireEvent.change(view.getByRole('searchbox', { name: 'Search chats' }), { target: { value: 'budget' } })
  expect(view.getByText('No chats match “budget”.')).toBeTruthy()
})

test('a row opens its chat', async () => {
  serve([{ conversations: [chat('c1', 'Launch plan', 5)], nextCursor: null }])
  const opened: string[] = []
  const view = show({ onOpen: (conversation) => opened.push(conversation.id) })
  fireEvent.click(await view.findByRole('button', { name: /^Launch plan/ }))
  expect(opened).toEqual(['c1'])
})

test('older chats load page by page', async () => {
  const calls = serve([
    { conversations: [chat('c1', 'Newest', 1)], nextCursor: '123.c1' },
    { conversations: [chat('c2', 'Oldest', 600)], nextCursor: null },
  ])
  const view = show()
  fireEvent.click(await view.findByRole('button', { name: 'Load older chats' }))
  expect(await view.findByText('Oldest')).toBeTruthy()
  expect(calls.some((call) => call.query === '?before=123.c1')).toBe(true)
  expect(view.queryByRole('button', { name: 'Load older chats' })).toBeNull()
})

test('rename saves the new title with Enter, and Escape cancels', async () => {
  const calls = serve([{ conversations: [chat('c1', 'Launch plan', 5)], nextCursor: null }])
  const view = show()
  fireEvent.click(await view.findByRole('button', { name: 'Rename “Launch plan”' }))
  const field = view.getByRole('textbox', { name: 'Chat name' }) as HTMLInputElement
  expect(field.value).toBe('Launch plan')
  fireEvent.keyDown(field, { key: 'Escape' })
  expect(view.queryByRole('textbox', { name: 'Chat name' })).toBeNull()
  expect(calls.filter((call) => call.method === 'PATCH')).toEqual([])

  fireEvent.click(view.getByRole('button', { name: 'Rename “Launch plan”' }))
  const again = view.getByRole('textbox', { name: 'Chat name' })
  fireEvent.change(again, { target: { value: 'Q4 launch' } })
  fireEvent.submit(again.closest('form') as HTMLFormElement)
  expect(await view.findByRole('button', { name: /^Q4 launch/ })).toBeTruthy()
  const patch = calls.find((call) => call.method === 'PATCH')
  expect(patch?.path).toBe(`/api/v1/assistant/workspaces/${W}/conversations/c1`)
  expect(patch?.body).toEqual({ title: 'Q4 launch' })
})

test('delete asks first, and Cancel keeps the chat', async () => {
  const calls = serve([{ conversations: [chat('c1', 'Launch plan', 5)], nextCursor: null }])
  const deleted: string[] = []
  const view = show({ onDeleted: (conversation) => deleted.push(conversation.id) })
  fireEvent.click(await view.findByRole('button', { name: 'Delete “Launch plan”' }))
  const dialog = await view.findByRole('alertdialog')
  expect(dialog.textContent).toContain('Delete this chat?')
  expect(dialog.textContent).toContain('“Launch plan” and its messages are deleted for good.')
  fireEvent.click(view.getByRole('button', { name: 'Cancel' }))
  expect(calls.filter((call) => call.method === 'DELETE')).toEqual([])
  expect(deleted).toEqual([])
  expect(view.getByText('Launch plan')).toBeTruthy()
})

test('Delete in the confirmation deletes the chat', async () => {
  const calls = serve([{ conversations: [chat('c1', 'Launch plan', 5), chat('c2', 'Budget', 9)], nextCursor: null }])
  const deleted: string[] = []
  const view = show({ onDeleted: (conversation) => deleted.push(conversation.id) })
  fireEvent.click(await view.findByRole('button', { name: 'Delete “Launch plan”' }))
  await view.findByRole('alertdialog')
  fireEvent.click(view.getByRole('button', { name: 'Delete' }))
  await waitFor(() => expect(deleted).toEqual(['c1']))
  expect(view.queryByText('Launch plan')).toBeNull()
  expect(view.getByText('Budget')).toBeTruthy()
  expect(calls.find((call) => call.method === 'DELETE')?.path).toBe(`/api/v1/assistant/workspaces/${W}/conversations/c1`)
})

test('a failed request shows its reason', async () => {
  serve([{ conversations: [chat('c1', 'Launch plan', 5)], nextCursor: null }])
  globalThis.fetch = (async (_input: RequestInfo | URL, init?: RequestInit) =>
    init?.method === 'DELETE'
      ? Response.json({ code: 'busy', detail: 'Ask AI is still working; stop it first.' }, { status: 409 })
      : Response.json({ conversations: [chat('c1', 'Launch plan', 5)], nextCursor: null })) as typeof fetch
  const view = show()
  fireEvent.click(await view.findByRole('button', { name: 'Delete “Launch plan”' }))
  await view.findByRole('alertdialog')
  fireEvent.click(view.getByRole('button', { name: 'Delete' }))
  expect((await view.findByRole('alert')).textContent).toBe('Ask AI is still working; stop it first.')
  expect(view.getByText('Launch plan')).toBeTruthy()
})

test('titles fall back and the filter needs every word', () => {
  expect(chatTitle({ title: null })).toBe('Untitled chat')
  expect(chatTitle({ title: '   ' })).toBe('Untitled chat')
  const rows = [chat('a', 'Launch plan for Q4', 1), chat('b', 'Plan the offsite', 2)]
  expect(filterConversations(rows, 'plan').map((row) => row.id)).toEqual(['a', 'b'])
  expect(filterConversations(rows, 'plan q4').map((row) => row.id)).toEqual(['a'])
  expect(filterConversations(rows, '  ').map((row) => row.id)).toEqual(['a', 'b'])
})
