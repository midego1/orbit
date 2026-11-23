import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { render, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import type { WorkspaceRole } from '@orbit/api/generated/types.gen'
import { WorkspaceContext } from '@/features/workspaces/workspaceContext'
import { testWorkspace } from '@/test/workspace'
import { AiPage } from './AiPage'

const originalFetch = globalThis.fetch
afterEach(() => {
  globalThis.fetch = originalFetch
})

const usage = { models: [], total: { requests: 3, input_tokens: 1000, output_tokens: 200, cache_read_tokens: 500, cache_write_tokens: 0 }, estimatedCostUsd: 0.0041, unpricedRequests: 0 }
const settings = (last4: string | null) => ({
  provider: 'anthropic',
  model: 'claude-sonnet-5-5',
  models: ['claude-sonnet-5-5', 'claude-opus-5-5', 'claude-haiku-5-5'],
  key: { set: last4 !== null, last4, source: last4 ? 'workspace' : null },
  appKey: true,
  usage: { today: usage, last30Days: usage },
  dailyTokenCap: 2_000_000,
  maxDailyTokenCap: 1_000_000_000,
  tokensToday: 1_050,
})

function renderPage(role: WorkspaceRole = 'owner') {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } })
  const workspace = testWorkspace(role)
  return render(
    <QueryClientProvider client={client}>
      <WorkspaceContext.Provider value={{ workspace, workspaces: [workspace], selectWorkspace: () => {} }}>
        <AiPage />
      </WorkspaceContext.Provider>
    </QueryClientProvider>,
  )
}

test('an admin saves a key; the page shows only its last four characters', async () => {
  const sent: unknown[] = []
  let stored: string | null = null
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    if (init?.method === 'PUT') {
      const body = JSON.parse(String(init.body)) as { apiKey?: string }
      sent.push(body)
      stored = body.apiKey?.slice(-4) ?? stored
    }
    void input
    return Response.json(settings(stored))
  }) as typeof fetch
  const view = renderPage()
  expect((await view.findByTestId('ai-key-state')).textContent).toContain('No key is set')
  await userEvent.type(view.getByLabelText(/^API key/), 'sk-ant-api03-abcdefghijklmnop-wxyz')
  await userEvent.click(view.getByRole('button', { name: 'Save' }))
  await waitFor(() => expect(sent).toEqual([{ model: 'claude-sonnet-5-5', dailyTokenCap: 2_000_000, apiKey: 'sk-ant-api03-abcdefghijklmnop-wxyz' }]))
  expect(view.getByTestId('ai-cap-used').textContent).toContain("Counted toward today's limit: 1,050 (cache reads count 10%)")
  await waitFor(() => expect(view.getByTestId('ai-key-state').textContent).toContain('…wxyz'))
  expect(view.queryByDisplayValue(/sk-ant/)).toBeNull()
  // each kind of input in its own column, and a rough cost under a cent
  const table = view.getByTestId('ai-usage').textContent ?? ''
  expect(table).toContain('1,000')
  expect(table).toContain('500')
  expect(table).toContain('~< $0.01')
  // the price note is a tooltip on the column, and no dated prices are shown on the page
  expect(view.getByTestId('ai-cost-head').textContent).toContain('Cost (rough)')
  expect(view.queryByText(/\$2 input|as of /)).toBeNull()
})

test('Test reports a refused key', async () => {
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = typeof input === 'string' ? input : input instanceof URL ? input.href : input.url
    if (url.endsWith('/settings/test') && init?.method === 'POST') return Response.json({ ok: false, error: "Anthropic refused this workspace's API key." })
    return Response.json(settings('abcd'))
  }) as typeof fetch
  const view = renderPage()
  await userEvent.click(await view.findByRole('button', { name: 'Test' }))
  expect((await view.findByTestId('ai-test-result')).textContent).toContain('refused')
})

test('a member cannot open the settings', async () => {
  let requested = false
  globalThis.fetch = (async () => {
    requested = true
    return Response.json({})
  }) as unknown as typeof fetch
  const view = renderPage('member')
  expect(await view.findByText(/Only workspace owners and admins/)).toBeTruthy()
  expect(requested).toBe(false)
})
