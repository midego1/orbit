import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { render } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import type { WorkspaceRole } from '@orbit/api/generated/types.gen'
import { WorkspaceContext } from '@/features/workspaces/workspaceContext'
import { testWorkspace } from '@/test/workspace'
import AssistantPanel from './AssistantPanel'

const originalFetch = globalThis.fetch
afterEach(() => {
  globalThis.fetch = originalFetch
})

function serve(status: unknown, onRequest?: (path: string, method: string) => void) {
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = typeof input === 'string' ? input : input instanceof URL ? input.href : input.url
    const path = new URL(url, 'http://orbit.test').pathname
    const method = init?.method ?? (input instanceof Request ? input.method : 'GET')
    onRequest?.(path, method)
    if (path.endsWith('/conversation')) return Response.json(status)
    // projects and statuses, for the names on approval cards
    return Response.json([])
  }) as typeof fetch
}

function renderPanel(role: WorkspaceRole = 'owner') {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } })
  const workspace = testWorkspace(role)
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <WorkspaceContext.Provider value={{ workspace, workspaces: [workspace], selectWorkspace: () => {} }}>
          <AssistantPanel onClose={() => {}} />
        </WorkspaceContext.Provider>
      </MemoryRouter>
    </QueryClientProvider>,
  )
}

test('without a key, an admin is sent to Settings → AI', async () => {
  serve({ conversation: null, setup: 'no_key', canManage: true, model: 'claude-sonnet-5-5' })
  const view = renderPanel()
  expect(await view.findByText('Ask AI needs an API key')).toBeTruthy()
  const link = view.getByRole('link', { name: 'Open Settings → AI' })
  expect(link.getAttribute('href')).toBe('/settings/ai')
})

test('without a key, a member is told who can add one', async () => {
  serve({ conversation: null, setup: 'no_key', canManage: false, model: 'claude-sonnet-5-5' })
  const view = renderPanel('member')
  expect(await view.findByText(/A workspace owner or admin can add it in Settings → AI/)).toBeTruthy()
  expect(view.queryByRole('link', { name: 'Open Settings → AI' })).toBeNull()
})

// The live conversation (a first visit, a reload with a waiting approval card, approve and decline) needs the AG-UI
// runtime's web streams, which the test DOM lacks; a real-browser click-through with the fake model covers it
// (scripts/assistant-fake-model.ts).
