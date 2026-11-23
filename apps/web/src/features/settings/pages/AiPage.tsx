import { useState, type FormEvent } from 'react'
import { toast } from 'sonner'
import { SettingsCard } from '@/components/common/SettingsCard'
import { InfoTip } from '@/components/common/InfoTip'
import { confirmAction } from '@/components/common/confirmAction'
import { Button } from '@/components/ui/button'
import { Field, FieldLabel } from '@/components/ui/field'
import { Input } from '@/components/ui/input'
import { PasswordInput } from '@/components/ui/password-input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { LoadingText } from '@/components/ui/spinner'
import { FieldGrid } from '@/features/settings/components/SettingsParts'
import { useCan } from '@/features/workspaces/permissions'
import { useWorkspace } from '@/features/workspaces/workspaceContext'
import { AssistantApiError, MODEL_LABELS, useAiSettings, useRemoveAiKey, useSaveAiSettings, useTestAiKey, type AiSettings, type UsageView } from '@/features/assistant/api'

function errorText(error: unknown, fallback: string): string {
  return error instanceof AssistantApiError ? error.message : fallback
}

/** Workspace settings for Ask AI: the provider's API key (bring your own), the model, and the token usage. Owners and admins only. */
export function AiPage() {
  const { workspace } = useWorkspace()
  const canManage = useCan('integrations.manage')
  const settings = useAiSettings(workspace.id, canManage)
  if (!canManage) {
    return (
      <SettingsCard title="Ask AI">
        <p className="text-sm text-muted-foreground">Only workspace owners and admins can change the AI settings.</p>
      </SettingsCard>
    )
  }
  if (settings.isPending) return <SettingsCard title="Ask AI"><p><LoadingText>Loading settings…</LoadingText></p></SettingsCard>
  if (settings.isError) {
    return (
      <SettingsCard title="Ask AI">
        <p role="alert">Settings could not be loaded. <Button variant="ghost" onClick={() => void settings.refetch()}>Retry</Button></p>
      </SettingsCard>
    )
  }
  return (
    <>
      <KeyForm key={`${settings.data.model}:${settings.data.key.last4 ?? ''}:${settings.data.dailyTokenCap}`} workspaceId={workspace.id} settings={settings.data} />
      <UsageCard settings={settings.data} />
    </>
  )
}

function KeyForm({ workspaceId, settings }: { workspaceId: string; settings: AiSettings }) {
  const [model, setModel] = useState(settings.model)
  const [apiKey, setApiKey] = useState('')
  const [cap, setCap] = useState(String(settings.dailyTokenCap))
  const [tested, setTested] = useState<string | null>(null)
  const save = useSaveAiSettings(workspaceId)
  const remove = useRemoveAiKey(workspaceId)
  const test = useTestAiKey(workspaceId)
  const options = settings.models.map((value) => ({ value, label: MODEL_LABELS[value] ?? value }))
  const typed = apiKey.trim()

  async function submit(event: FormEvent) {
    event.preventDefault()
    const dailyTokenCap = Number(cap.replace(/[\s,._]/g, ''))
    if (!Number.isInteger(dailyTokenCap) || dailyTokenCap < 1 || dailyTokenCap > settings.maxDailyTokenCap) {
      toast.error(`The daily token limit must be a whole number between 1 and ${numberFormat.format(settings.maxDailyTokenCap)}.`)
      return
    }
    try {
      await save.mutateAsync({ model, dailyTokenCap, ...(typed ? { apiKey: typed } : {}) })
      setApiKey('')
      toast.success('AI settings saved')
    } catch (error) {
      toast.error(errorText(error, 'The settings could not be saved.'))
    }
  }

  async function runTest() {
    setTested(null)
    try {
      const result = await test.mutateAsync(typed || undefined)
      if (result.ok) {
        const usable = result.available.includes(model)
        setTested(usable ? 'The key works.' : `The key works, but it cannot use ${MODEL_LABELS[model] ?? model}.`)
      } else {
        setTested(result.error)
      }
    } catch (error) {
      setTested(errorText(error, 'The key could not be tested.'))
    }
  }

  async function removeKey() {
    const confirmed = await confirmAction({
      title: 'Remove the API key?',
      description: 'Ask AI stops working in this workspace until someone adds a key again.',
      confirmLabel: 'Remove key',
      danger: true,
    })
    if (!confirmed) return
    try {
      await remove.mutateAsync()
      toast.success('API key removed')
    } catch (error) {
      toast.error(errorText(error, 'The key could not be removed.'))
    }
  }

  const keyLine = settings.key.source === 'workspace'
    ? `A key ending in …${settings.key.last4} is set.`
    : settings.key.source === 'server'
      ? 'The server provides a key (ORBIT_AI_ANTHROPIC_KEY). A key set here replaces it for this workspace.'
      : 'No key is set: Ask AI does not work in this workspace yet.'

  return (
    <SettingsCard title="Ask AI" description="Ask AI uses Anthropic's Claude models with this workspace's own API key. Each request is billed to that key, and what Ask AI reads for a request (tasks, docs, chat, your name and email) is sent to Anthropic. The key is stored encrypted and is never shown again; only its last four characters are.">
      <form className="flex flex-col gap-4" onSubmit={(event) => void submit(event)}>
        <p className="text-sm text-foreground" data-testid="ai-key-state">{keyLine}</p>
        {!settings.appKey ? (
          <p role="alert" className="text-sm text-destructive">This server has no ORBIT_APP_KEY, so it cannot store an API key. Ask the server's operator to set one.</p>
        ) : null}
        <FieldGrid>
          <Field>
            <FieldLabel htmlFor="ai-provider">Provider</FieldLabel>
            <p id="ai-provider" className="text-sm text-foreground">Anthropic</p>
          </Field>
          <Field>
            <FieldLabel htmlFor="ai-model" className="items-center gap-1.5">Model <InfoTip text="Sonnet suits most work. Opus is the most capable and costs about twice as much; Haiku is the fastest and cheapest." /></FieldLabel>
            <Select items={options} value={model} onValueChange={(value) => setModel(value as string)}>
              <SelectTrigger id="ai-model">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {options.map((option) => <SelectItem key={option.value} value={option.value}>{option.label}</SelectItem>)}
              </SelectContent>
            </Select>
          </Field>
          <Field>
            <FieldLabel htmlFor="ai-cap" className="items-center gap-1.5">
              Daily token limit
              <InfoTip text="An approximate limit on tokens, not money: the most tokens the whole workspace may use per day (UTC). Cache reads count for a tenth, as they cost a tenth. Each request counts at an estimate while it runs, so the limit can be passed by about one request. Output tokens and Opus cost more, so the same limit costs more there. When the limit is reached, Ask AI stops until the next day." />
            </FieldLabel>
            <Input id="ai-cap" inputMode="numeric" value={cap} onChange={(event) => setCap(event.target.value)} />
            <p className="text-xs text-muted-foreground" data-testid="ai-cap-used">
              Counted toward today's limit: {numberFormat.format(settings.tokensToday)} (cache reads count 10%){' '}
              <InfoTip text="The limit weighs tokens by what they cost: new input, cache writes and output count in full, cache reads (the earlier part of the conversation, read again on each step) count for a tenth. So this number is lower than the sum of the columns in Usage below. A request that is still running counts at an estimate." />
              {settings.key.source === 'server' ? ` · at most ${numberFormat.format(settings.maxDailyTokenCap)} with the server's key` : ''}
            </p>
          </Field>
          <Field>
            <FieldLabel htmlFor="ai-key" className="items-center gap-1.5">
              {settings.key.source === 'workspace' ? 'Replace API key' : 'API key'}
              <InfoTip text="Create a key in the Anthropic Console (console.anthropic.com → API keys). It starts with sk-ant-." />
            </FieldLabel>
            <PasswordInput id="ai-key" autoComplete="off" spellCheck={false} placeholder="sk-ant-…" value={apiKey} onChange={(event) => setApiKey(event.target.value)} />
          </Field>
        </FieldGrid>
        {tested ? <p className="text-sm text-foreground" role="status" data-testid="ai-test-result">{tested}</p> : null}
        <div className="flex flex-wrap items-center gap-2">
          <Button type="submit" disabled={save.isPending || !settings.appKey && !!typed}>{save.isPending ? 'Saving…' : 'Save'}</Button>
          <Button type="button" variant="outline" disabled={test.isPending || (!typed && !settings.key.set)} onClick={() => void runTest()}>
            {test.isPending ? 'Testing…' : 'Test'}
          </Button>
          {settings.key.source === 'workspace' ? (
            <Button type="button" variant="ghost" disabled={remove.isPending} onClick={() => void removeKey()}>Remove key</Button>
          ) : null}
        </div>
      </form>
    </SettingsCard>
  )
}

const numberFormat = new Intl.NumberFormat()

function formatCost(dollars: number): string {
  if (dollars === 0) return '$0.00'
  if (dollars < 0.01) return '< $0.01'
  return `$${dollars.toFixed(2)}`
}

function UsageRow({ label, usage }: { label: string; usage: UsageView }) {
  const cell = 'py-2 pr-4 text-right tabular-nums'
  return (
    <tr className="border-t border-border/60">
      <th scope="row" className="py-2 pr-4 text-left font-medium">{label}</th>
      <td className={cell}>{numberFormat.format(usage.total.requests)}</td>
      <td className={cell}>{numberFormat.format(usage.total.input_tokens)}</td>
      <td className={cell}>{numberFormat.format(usage.total.cache_read_tokens)}</td>
      <td className={cell}>{numberFormat.format(usage.total.cache_write_tokens)}</td>
      <td className={cell}>{numberFormat.format(usage.total.output_tokens)}</td>
      <td className="py-2 text-right tabular-nums">
        ~{formatCost(usage.estimatedCostUsd)}
        {usage.unpricedRequests ? <span className="block text-xs text-muted-foreground">{usage.unpricedRequests} not priced</span> : null}
      </td>
    </tr>
  )
}

function UsageCard({ settings }: { settings: AiSettings }) {
  const head = 'pb-2 pr-4 text-right font-normal'
  return (
    <SettingsCard title="Usage" description="Model requests made by Ask AI in this workspace, with every member's requests counted, as Anthropic reported them. A request that is still running counts at an estimate until it ends.">
      <div className="overflow-x-auto">
        <table className="w-full text-sm" data-testid="ai-usage">
          <thead className="text-xs text-muted-foreground">
            <tr>
              <th className="pb-2 text-left font-normal"><span className="sr-only">Period</span></th>
              <th className={head}>Requests</th>
              <th className={head}>Input <InfoTip text="New input, billed at the full input price. Anthropic counts cache reads and cache writes separately (the next two columns), so they are not in this number." /></th>
              <th className={head}>Cache reads <InfoTip text="Input read from Anthropic's prompt cache: the earlier part of the conversation, billed at a small part of the input price." /></th>
              <th className={head}>Cache writes <InfoTip text="Input written to the prompt cache for the next steps, billed at 1.25 times the input price." /></th>
              <th className={head}>Output <InfoTip text="Answers and tool calls, including the model's thinking, billed at the output price." /></th>
              <th className="pb-2 text-right font-normal" data-testid="ai-cost-head">Cost (rough) <InfoTip text="A rough estimate at Anthropic's list prices for each model (the server keeps the rate table). Cache reads cost a small part of the input price, cache writes 1.25 times input. Prices change and discounts are not included: your Anthropic invoice is what counts." /></th>
            </tr>
          </thead>
          <tbody>
            <UsageRow label="Today (UTC)" usage={settings.usage.today} />
            <UsageRow label="Last 30 days" usage={settings.usage.last30Days} />
          </tbody>
        </table>
      </div>
    </SettingsCard>
  )
}
