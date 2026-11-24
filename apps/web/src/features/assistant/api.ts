// Ask AI's server API: the current conversation, the member's history of earlier ones (kept server-side for 30 days,
// for that member only), a new chat, Stop, and the workspace's AI settings (bring your own Anthropic key). Runs
// themselves go over AG-UI (AssistantPanel.tsx).
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'

export type AssistantSetup = 'ready' | 'no_key' | 'no_app_key'

export type AssistantConversation = {
  id: string
  /** The first message, cut short, or the member's own name for the chat; null before the first message. */
  title: string | null
  /** AG-UI messages, with the waiting approval's interrupt in the assistant message's metadata. */
  messages: unknown[]
  messageCount: number
  /** The member's current conversation (the others are history; only the active one runs). */
  active: boolean
  createdAt: number
  updatedAt: number
  expiresAt: number
  busy: boolean
  /** The model this chat's next run uses (the server checks it against the allowed models on every run). */
  model: string
}

/** A conversation as the history lists it. */
export type ConversationSummary = {
  id: string
  title: string | null
  messageCount: number
  active: boolean
  createdAt: number
  updatedAt: number
}

export type ConversationPage = { conversations: ConversationSummary[]; nextCursor: string | null }

export type AssistantStatus = {
  conversation: AssistantConversation | null
  setup: AssistantSetup
  canManage: boolean
  /** The workspace's model. */
  model: string
  /** The models a member may pick for a chat: the workspace's model first. */
  models: string[]
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
  /** The models members may pick per chat: the workspace's model first, then the ones an admin allowed. */
  allowedModels: string[]
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
  history: (workspaceId: string) => ['assistant', workspaceId, 'history'] as const,
  conversation: (workspaceId: string, id: string) => ['assistant', workspaceId, 'conversation', id] as const,
}

export function fetchAssistantStatus(workspaceId: string): Promise<AssistantStatus> {
  return request(`${assistantBase(workspaceId)}/conversation`)
}

export function startConversation(workspaceId: string): Promise<{ conversation: AssistantConversation }> {
  return request(`${assistantBase(workspaceId)}/conversations`, { method: 'POST' })
}

function conversationPath(workspaceId: string, conversationId: string): string {
  return `${assistantBase(workspaceId)}/conversations/${encodeURIComponent(conversationId)}`
}

/** One page of the member's conversations, newest use first; `before` is the previous page's `nextCursor`. */
export function fetchConversations(workspaceId: string, before?: string | null): Promise<ConversationPage> {
  const query = before ? `?before=${encodeURIComponent(before)}` : ''
  return request(`${assistantBase(workspaceId)}/conversations${query}`)
}

export function openConversation(workspaceId: string, conversationId: string): Promise<{ conversation: AssistantConversation }> {
  return request(conversationPath(workspaceId, conversationId))
}

export function renameConversation(workspaceId: string, conversationId: string, title: string): Promise<{ conversation: ConversationSummary }> {
  return request(conversationPath(workspaceId, conversationId), { method: 'PATCH', body: JSON.stringify({ title }) })
}

export function deleteConversation(workspaceId: string, conversationId: string): Promise<void> {
  return request(conversationPath(workspaceId, conversationId), { method: 'DELETE' })
}

/** Makes an earlier conversation the active one again; the one that was active is archived. */
export function continueConversation(workspaceId: string, conversationId: string): Promise<{ conversation: AssistantConversation }> {
  return request(`${conversationPath(workspaceId, conversationId)}/continue`, { method: 'POST' })
}

export function stopConversation(workspaceId: string, conversationId: string): Promise<void> {
  return request(`${assistantBase(workspaceId)}/conversations/${encodeURIComponent(conversationId)}/stop`, { method: 'POST' })
}

/** Picks the model of the user's own chat; the server refuses a model the workspace does not allow. */
export function setChatModel(workspaceId: string, conversationId: string, model: string): Promise<{ model: string; models: string[] }> {
  return request(`${assistantBase(workspaceId)}/conversations/${encodeURIComponent(conversationId)}/model`, {
    method: 'PUT',
    body: JSON.stringify({ model }),
  })
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
    mutationFn: (body: { model: string; apiKey?: string; dailyTokenCap?: number; allowedModels?: string[] }) =>
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

/** The short name the panel shows ("Sonnet 5.5"). */
export const MODEL_NAMES: Record<string, string> = {
  'claude-sonnet-5-5': 'Sonnet 5.5',
  'claude-opus-5-5': 'Opus 5.5',
  'claude-haiku-5-5': 'Haiku 5.5',
}

export function modelName(model: string): string {
  return MODEL_NAMES[model] ?? model
}
