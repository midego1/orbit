import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { View } from 'react-native'

import { ConfirmDialog, type ConfirmRequest } from '../../../components/settings/Dialog'
import { CheckRow } from '../../../components/settings/ProjectPicker'
import { FieldLabel, Note, Select, SettingsCard, SettingsShell, WORKSPACE_NAV } from '../../../components/settings/Shell'
import * as api from '../../../data/ai-settings-api'
import { useOrbit } from '../../../orbit-context'
import { Button, Input, LabelTip, Text } from '../../../ui'

const DESCRIPTION =
  "Ask AI uses Anthropic's Claude models with this workspace's own API key. Each request is billed to that key, and what Ask AI reads for a request (tasks, docs, chat, your name and email) is sent to Anthropic. The key is stored encrypted and is never shown again; only its last four characters are."

const numbers = new Intl.NumberFormat()

/** The web's Settings → AI. Ask AI's chat panel itself is on the web only for now. */
export default function AiSettingsScreen() {
  const { client, workspace, workspaceId } = useOrbit()
  const canManage = workspace.permissions.includes('integrations.manage')
  const settings = useQuery({ queryKey: api.aiSettingsKeys.settings(workspaceId), queryFn: () => api.aiSettings(client, workspaceId), enabled: canManage })
  return (
    <SettingsShell title="Settings" nav={WORKSPACE_NAV} onRefresh={canManage ? () => settings.refetch() : undefined}>
      {!canManage ? (
        <SettingsCard title="Ask AI">
          <Text className="text-[14px] leading-[20px] text-muted-foreground">Only workspace owners and admins can change the AI settings.</Text>
        </SettingsCard>
      ) : settings.data ? (
        <>
          <KeyCard key={`${settings.data.model}:${settings.data.key.last4 ?? ''}:${settings.data.dailyTokenCap}:${settings.data.allowedModels.join(',')}`} settings={settings.data} />
          <UsageCard settings={settings.data} />
        </>
      ) : (
        <SettingsCard title="Ask AI" description={DESCRIPTION}>
          {settings.isError ? (
            <View className="flex-row items-center gap-2">
              <Note error>The AI settings could not be loaded.</Note>
              <Button label="Retry" variant="ghost" onPress={() => void settings.refetch()} />
            </View>
          ) : (
            <Note loading>Loading settings…</Note>
          )}
        </SettingsCard>
      )}
      <SettingsCard title="On this phone">
        <Note>Ask AI's chat panel is available in Orbit on the web (the Ask AI row in the sidebar, or Ctrl+J). The app has these settings only for now.</Note>
      </SettingsCard>
    </SettingsShell>
  )
}

function KeyCard({ settings }: { settings: api.AiSettings }) {
  const { client, workspaceId } = useOrbit()
  const queryClient = useQueryClient()
  const stored = (next: api.AiSettings) => queryClient.setQueryData(api.aiSettingsKeys.settings(workspaceId), next)
  const save = useMutation({ mutationFn: (body: api.AiSettingsBody) => api.saveAiSettings(client, workspaceId, body), onSuccess: stored })
  const remove = useMutation({ mutationFn: () => api.removeAiKey(client, workspaceId), onSuccess: stored })
  const test = useMutation({ mutationFn: (apiKey?: string) => api.testAiKey(client, workspaceId, apiKey) })
  const [model, setModel] = useState(settings.model)
  // the models members may pick per chat, besides the workspace's model (which is always allowed)
  const [allowed, setAllowed] = useState(() => api.chatModelsToSave(settings.allowedModels, settings.model))
  const [apiKey, setApiKey] = useState('')
  const [cap, setCap] = useState(String(settings.dailyTokenCap))
  const [message, setMessage] = useState<{ text: string; error: boolean }>()
  const [confirm, setConfirm] = useState<ConfirmRequest | null>(null)
  const typed = apiKey.trim()
  const options = settings.models.map((value) => ({ value, label: api.MODEL_LABELS[value] ?? value }))

  const onSave = () => {
    const dailyTokenCap = api.parseTokenCap(cap, settings.maxDailyTokenCap)
    if (dailyTokenCap === null) {
      setMessage({ text: `The daily token limit must be a whole number between 1 and ${numbers.format(settings.maxDailyTokenCap)}.`, error: true })
      return
    }
    setMessage(undefined)
    save.mutate(
      { model, dailyTokenCap, allowedModels: api.chatModelsToSave(allowed, model), ...(typed ? { apiKey: typed } : {}) },
      {
        onSuccess: () => {
          setApiKey('')
          setMessage({ text: 'AI settings saved.', error: false })
        },
        onError: (error) => setMessage({ text: api.aiSettingsFailure(error, 'The settings could not be saved.'), error: true }),
      },
    )
  }
  const onTest = () => {
    setMessage(undefined)
    test.mutate(typed || undefined, {
      onSuccess: (result) =>
        setMessage(
          result.ok
            ? { text: result.available.includes(model) ? 'The key works.' : `The key works, but it cannot use ${api.MODEL_LABELS[model] ?? model}.`, error: false }
            : { text: result.error, error: true },
        ),
      onError: (error) => setMessage({ text: api.aiSettingsFailure(error, 'The key could not be tested.'), error: true }),
    })
  }
  const onRemove = () =>
    setConfirm({
      title: 'Remove the API key?',
      description: 'Ask AI stops working in this workspace until someone adds a key again.',
      confirmLabel: 'Remove key',
      danger: true,
      onConfirm: () => remove.mutate(undefined, { onError: (error) => setMessage({ text: api.aiSettingsFailure(error, 'The key could not be removed.'), error: true }) }),
    })

  return (
    <SettingsCard title="Ask AI" description={DESCRIPTION}>
      <View className="gap-4">
        <Text className="text-[14px] leading-[20px]">{api.keyLine(settings.key)}</Text>
        {!settings.appKey ? <Note error>This server has no ORBIT_APP_KEY, so it cannot store an API key. Ask the server's operator to set one.</Note> : null}
        <View className="gap-1.5">
          <FieldLabel>Provider</FieldLabel>
          <Text className="text-[14px] leading-[20px]">Anthropic</Text>
        </View>
        <View className="gap-1.5">
          <LabelTip label="Model" text="Sonnet suits most work. Opus is the most capable and costs about twice as much; Haiku is the fastest and cheapest." />
          <Select label="Model" value={model} options={options} onChange={setModel} />
        </View>
        <View className="gap-2">
          <LabelTip
            label="Models members may pick per chat"
            text="Members pick one of these for a chat with /model or the model menu in the Ask AI panel (on the web). The model above is always allowed and is what a new chat uses. If you take a model off the list, chats that use it go back to the model above on their next message, and the member is told."
          />
          {settings.models.map((value) => {
            const isDefault = value === model
            return (
              <CheckRow
                key={value}
                label={`${api.MODEL_NAMES[value] ?? value}${isDefault ? " (the workspace's model)" : ''}`}
                checked={isDefault || allowed.includes(value)}
                muted={isDefault}
                onPress={() => {
                  if (isDefault) return
                  setAllowed(allowed.includes(value) ? allowed.filter((other) => other !== value) : [...allowed, value])
                }}
              />
            )
          })}
        </View>
        <Input
          label="Daily token limit"
          help="An approximate limit on tokens, not money: the most tokens the whole workspace may use per day (UTC). Cache reads count for a tenth. Each request counts at an estimate while it runs, so the limit can be passed by about one request. When the limit is reached, Ask AI stops until the next day."
          keyboardType="number-pad"
          value={cap}
          onChangeText={setCap}
          description={`Counted toward today's limit: ${numbers.format(settings.tokensToday)} (cache reads count 10%)${settings.key.source === 'server' ? ` · at most ${numbers.format(settings.maxDailyTokenCap)} with the server's key` : ''}`}
        />
        <Input
          label={settings.key.source === 'workspace' ? 'Replace API key' : 'API key'}
          help={settings.gateway ? 'This server sends Ask AI through a gateway: enter the key the gateway gave this workspace. It starts with sk-.' : 'Create a key in the Anthropic Console (console.anthropic.com → API keys). It starts with sk-ant-.'}
          placeholder={settings.gateway ? 'sk-…' : 'sk-ant-…'}
          autoCapitalize="none"
          autoCorrect={false}
          secureTextEntry
          value={apiKey}
          onChangeText={setApiKey}
        />
        {message ? <Note error={message.error}>{message.text}</Note> : null}
        <View className="flex-row flex-wrap gap-2">
          <Button label="Save" loading={save.isPending} disabled={!settings.appKey && !!typed} onPress={onSave} />
          <Button label="Test" variant="outline" loading={test.isPending} disabled={!typed && !settings.key.set} onPress={onTest} />
          {settings.key.source === 'workspace' ? <Button label="Remove key" variant="ghost" loading={remove.isPending} onPress={onRemove} /> : null}
        </View>
      </View>
      <ConfirmDialog request={confirm} onClose={() => setConfirm(null)} />
    </SettingsCard>
  )
}

function UsageCard({ settings }: { settings: api.AiSettings }) {
  const rows = [
    { label: 'Today (UTC)', usage: settings.usage.today },
    { label: 'Last 30 days', usage: settings.usage.last30Days },
  ]
  return (
    <SettingsCard title="Usage" description="Model requests made by Ask AI in this workspace, with every member's requests counted, as Anthropic reported them. Costs are a rough estimate at Anthropic's list prices: your Anthropic invoice is what counts.">
      <View className="gap-3">
        {rows.map(({ label, usage }) => (
          <View key={label} className="gap-0.5">
            <Text className="font-noto-medium text-[14px] leading-[20px]">{label}</Text>
            <Text className="text-[13px] leading-[19px] text-muted-foreground">
              {numbers.format(usage.total.requests)} requests · input {numbers.format(usage.total.input_tokens)} · cache reads {numbers.format(usage.total.cache_read_tokens)} · cache writes {numbers.format(usage.total.cache_write_tokens)} · output {numbers.format(usage.total.output_tokens)}
            </Text>
            <Text className="text-[13px] leading-[19px]">
              ~{api.formatCost(usage.estimatedCostUsd)}
              {usage.unpricedRequests ? ` (${usage.unpricedRequests} not priced)` : ''}
            </Text>
          </View>
        ))}
      </View>
    </SettingsCard>
  )
}
