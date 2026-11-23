// Ask AI's server API: the current conversation (kept server-side for a few hours), a new chat, Stop, and the
// workspace's AI settings (bring your own Anthropic key). Runs themselves go over AG-UI (AssistantPanel.tsx).
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'

export type AssistantSetup = 'ready' | 'no_key' | 'no_app_key'

export type AssistantConversation = {
  id: string
  /** AG-UI messages, with the waiting approval's interrupt in the assistant message's metadata. */
  messages: unknown[]
  expiresAt: number
  busy: boolean
}

export type AssistantStatus = {
  conversation: AssistantConversation | null
  setup: AssistantSetup
  canManage: boolean
  model: string
}

export type UsageTotals = {
  requests: number
  input_tokens: number
  output_tokens: number
  cache_read_tokens: number
  cache_write_tokens: number
}

export type UsageView = {
  models: (UsageTotals & { model: string })[]
  total: UsageTotals
  /** Dollars at Anthropic's list prices; a rough estimate. */
  estimatedCostUsd: number
  /** Requests answered by a model without known prices (not in the estimate). */
  unpricedRequests: number
}

export type AiSettings = {
  provider: 'anthropic'
  model: string
  models: string[]
  key: { set: boolean; last4: string | null; source: 'workspace' | 'server' | null }
  appKey: boolean
  usage: { today: UsageView; last30Days: UsageView }
  /** Tokens per UTC day for the whole workspace; cache reads count a tenth. */
  dailyTokenCap: number
  /** The highest limit this workspace may set (lower while it uses the server's key). */
  maxDailyTokenCap: number
  /** Today's tokens, counted the same way (requests in flight at their estimate). */
  tokensToday: number
}

export type KeyTest = { ok: true; available: string[]; models: string[] } | { ok: false; error: string }

export class AssistantApiError extends Error {
  readonly status: number
  readonly code: string

  constructor(status: number, code: string, detail: string) {
    super(detail)
    this.status = status
    this.code = code
  }
}

export function assistantBase(workspaceId: string): string {
  return `/api/v1/assistant/workspaces/${encodeURIComponent(workspaceId)}`
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(path, {
    credentials: 'same-origin',
    ...init,
    headers: { Accept: 'application/json', ...(init?.body ? { 'Content-Type': 'application/json' } : {}), ...init?.headers },
  })
  if (!response.ok) {
    let code = 'error'
    let detail = 'The request failed. Try again.'
    try {
      const body = (await response.json()) as { code?: string; detail?: string }
      code = body.code ?? code
      detail = body.detail ?? detail
    } catch {
      // not JSON
    }
    throw new AssistantApiError(response.status, code, detail)
  }
  if (response.status === 204) return undefined as T
  return (await response.json()) as T
}

export const assistantKeys = {
  status: (workspaceId: string) => ['assistant', workspaceId, 'status'] as const,
  settings: (workspaceId: string) => ['assistant', workspaceId, 'settings'] as const,
}

export function fetchAssistantStatus(workspaceId: string): Promise<AssistantStatus> {
  return request(`${assistantBase(workspaceId)}/conversation`)
}

export function startConversation(workspaceId: string): Promise<{ conversation: AssistantConversation }> {
  return request(`${assistantBase(workspaceId)}/conversations`, { method: 'POST' })
}

export function stopConversation(workspaceId: string, conversationId: string): Promise<void> {
  return request(`${assistantBase(workspaceId)}/conversations/${encodeURIComponent(conversationId)}/stop`, { method: 'POST' })
}

export function useAiSettings(workspaceId: string, enabled: boolean) {
  return useQuery({
    queryKey: assistantKeys.settings(workspaceId),
    queryFn: () => request<AiSettings>(`${assistantBase(workspaceId)}/settings`),
    enabled,
  })
}

export function useSaveAiSettings(workspaceId: string) {
  const client = useQueryClient()
  return useMutation({
    mutationFn: (body: { model: string; apiKey?: string; dailyTokenCap?: number }) =>
      request<AiSettings>(`${assistantBase(workspaceId)}/settings`, { method: 'PUT', body: JSON.stringify(body) }),
    onSuccess: (settings) => {
      client.setQueryData(assistantKeys.settings(workspaceId), settings)
      void client.invalidateQueries({ queryKey: assistantKeys.status(workspaceId) })
    },
  })
}

export function useRemoveAiKey(workspaceId: string) {
  const client = useQueryClient()
  return useMutation({
    mutationFn: () => request<AiSettings>(`${assistantBase(workspaceId)}/settings/key`, { method: 'DELETE' }),
    onSuccess: (settings) => {
      client.setQueryData(assistantKeys.settings(workspaceId), settings)
      void client.invalidateQueries({ queryKey: assistantKeys.status(workspaceId) })
    },
  })
}

export function useTestAiKey(workspaceId: string) {
  return useMutation({
    mutationFn: (apiKey?: string) =>
      request<KeyTest>(`${assistantBase(workspaceId)}/settings/test`, { method: 'POST', body: JSON.stringify(apiKey ? { apiKey } : {}) }),
  })
}

export const MODEL_LABELS: Record<string, string> = {
  'claude-sonnet-5-5': 'Claude Sonnet 5.5 (recommended)',
  'claude-opus-5-5': 'Claude Opus 5.5 (most capable, about twice the price)',
  'claude-haiku-5-5': 'Claude Haiku 5.5 (fastest and cheapest)',
}
