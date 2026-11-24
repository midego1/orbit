/** Ask AI's workspace settings (the web's Settings → AI): the Anthropic key, the model, the daily token limit and the usage. No React. */
import { ApiProblem } from '@orbit/api/problem'

import type { OrbitClient } from './orbit'

export type UsageTotals = { requests: number; input_tokens: number; output_tokens: number; cache_read_tokens: number; cache_write_tokens: number }
export type UsageView = { models: (UsageTotals & { model: string })[]; total: UsageTotals; estimatedCostUsd: number; unpricedRequests: number }

export type AiSettings = {
  provider: 'anthropic'
  model: string
  models: string[]
  /** The models members may pick per chat: the workspace's model first, then the ones an admin allowed. */
  allowedModels: string[]
  key: { set: boolean; last4: string | null; source: 'workspace' | 'server' | null }
  appKey: boolean
  usage: { today: UsageView; last30Days: UsageView }
  dailyTokenCap: number
  maxDailyTokenCap: number
  tokensToday: number
}

export type KeyTest = { ok: true; available: string[]; models: string[] } | { ok: false; error: string }

export type AiSettingsBody = { model: string; apiKey?: string; dailyTokenCap?: number; allowedModels?: string[] }

const base = '/api/v1/assistant/workspaces/{workspace_id}'

export const aiSettingsKeys = { settings: (workspaceId: string) => ['assistant', workspaceId, 'settings'] as const }

export async function aiSettings(client: OrbitClient, workspaceId: string): Promise<AiSettings> {
  return (await client.get<{ 200: AiSettings }, unknown, true>({ url: `${base}/settings`, path: { workspace_id: workspaceId }, throwOnError: true })).data
}

export async function saveAiSettings(client: OrbitClient, workspaceId: string, body: AiSettingsBody): Promise<AiSettings> {
  return (await client.put<{ 200: AiSettings }, unknown, true>({ url: `${base}/settings`, path: { workspace_id: workspaceId }, body, headers: { 'Content-Type': 'application/json' }, throwOnError: true })).data
}

export async function removeAiKey(client: OrbitClient, workspaceId: string): Promise<AiSettings> {
  return (await client.delete<{ 200: AiSettings }, unknown, true>({ url: `${base}/settings/key`, path: { workspace_id: workspaceId }, throwOnError: true })).data
}

/** Tests the typed key, or the stored one without it. Free: it only lists the models the key may use. */
export async function testAiKey(client: OrbitClient, workspaceId: string, apiKey?: string): Promise<KeyTest> {
  return (await client.post<{ 200: KeyTest }, unknown, true>({ url: `${base}/settings/test`, path: { workspace_id: workspaceId }, body: apiKey ? { apiKey } : {}, headers: { 'Content-Type': 'application/json' }, throwOnError: true })).data
}

export const MODEL_LABELS: Record<string, string> = {
  'claude-sonnet-5-5': 'Claude Sonnet 5.5 (recommended)',
  'claude-opus-5-5': 'Claude Opus 5.5 (most capable, about twice the price)',
  'claude-haiku-5-5': 'Claude Haiku 5.5 (fastest and cheapest)',
}

/** The short name of a model ("Sonnet 5.5"), as the web's chat model menu shows it. */
export const MODEL_NAMES: Record<string, string> = {
  'claude-sonnet-5-5': 'Sonnet 5.5',
  'claude-opus-5-5': 'Opus 5.5',
  'claude-haiku-5-5': 'Haiku 5.5',
}

/** The other models members may pick per chat, to save: the workspace's model is always allowed, so it is left out. */
export function chatModelsToSave(allowed: string[], model: string): string[] {
  return allowed.filter((other, index) => other !== model && allowed.indexOf(other) === index)
}

/** The server's own message (it never contains the key), or the fallback. */
export function aiSettingsFailure(error: unknown, fallback: string): string {
  return error instanceof ApiProblem && error.message ? error.message : fallback
}

/** The line about the key, as on the web. */
export function keyLine(key: AiSettings['key']): string {
  if (key.source === 'workspace') return `A key ending in …${key.last4} is set.`
  if (key.source === 'server') return 'The server provides a key (ORBIT_AI_ANTHROPIC_KEY). A key set here replaces it for this workspace.'
  return 'No key is set: Ask AI does not work in this workspace yet.'
}

/** The typed limit as a whole number, or null when it is not one between 1 and `max`. Spaces and separators are allowed. */
export function parseTokenCap(text: string, max: number): number | null {
  const value = Number(text.replace(/[\s,._]/g, ''))
  return Number.isInteger(value) && value >= 1 && value <= max ? value : null
}

export function formatCost(dollars: number): string {
  if (dollars === 0) return '$0.00'
  if (dollars < 0.01) return '< $0.01'
  return `$${dollars.toFixed(2)}`
}
