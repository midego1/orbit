// The approval of a batch of changes (`propose_changes`): one group with a card per change, each with a checkbox (all
// on at first), "Approve N selected" and "Decline all". The cards come from the server's stored batch (its item ids,
// exact arguments and previews, `metadata.batch`); the answer names only the ids the user keeps.
import { useState } from 'react'
import { cn } from 'cn'
import { InfoTip } from '@/components/common/InfoTip'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { Fields, PreviewCard } from './AssistantCards'
import { detailsOpen, type BatchItem } from './cards'
import { describeStep } from './toolSteps'

export type BatchDecision = { approved: true; items: string[] } | { approved: false }

export function BatchApprovalCard({ items, names, onDecide }: { items: BatchItem[]; names?: ReadonlyMap<string, string>; onDecide: (decision: BatchDecision) => void | Promise<void> }) {
  const [selected, setSelected] = useState<ReadonlySet<string>>(() => new Set(items.map((item) => item.id)))
  const [sent, setSent] = useState(false)
  const count = items.filter((item) => selected.has(item.id)).length
  const all = count === items.length
  const dangerous = items.some((item) => item.risk === 'dangerous')

  function toggle(id: string, on: boolean) {
    setSelected((old) => {
      const next = new Set(old)
      if (on) next.add(id)
      else next.delete(id)
      return next
    })
  }

  function decide(decision: BatchDecision) {
    if (sent) return
    setSent(true)
    // the answer did not go through (a lost connection): the card can be answered again
    try {
      void Promise.resolve(onDecide(decision)).catch(() => setSent(false))
    } catch {
      setSent(false)
    }
  }

  return (
    <section data-assistant-approval data-assistant-batch aria-label={`Approve ${items.length} changes`} className="flex flex-col rounded-lg bg-card shadow-[0_0_0_1px_var(--border)]">
      <header className="flex flex-col gap-0.5 px-3 pt-2.5 pb-2">
        <p className="text-[0.8125rem] font-medium text-foreground">{items.length === 1 ? 'Ask AI wants to make 1 change' : `Ask AI wants to make ${items.length} changes`}</p>
        <p className={cn('text-xs', dangerous ? 'text-amber-700 dark:text-amber-400' : 'text-muted-foreground')}>
          {dangerous ? 'Some change existing data. Untick what you do not want.' : 'Untick what you do not want.'}
        </p>
      </header>
      <div className="flex flex-col gap-2 rounded-b-lg border-t bg-background px-3 py-2.5">
        <div className="flex items-center gap-2 text-xs text-muted-foreground">
          <Checkbox
            checked={all}
            indeterminate={count > 0 && !all}
            aria-label={all ? 'Select none' : 'Select all'}
            disabled={sent}
            onCheckedChange={() => setSelected(all ? new Set() : new Set(items.map((item) => item.id)))}
          />
          <span aria-live="polite">{`${count} of ${items.length} selected`}</span>
        </div>
        <ol className="flex flex-col gap-2">
          {items.map((item, index) => (
            <BatchRow key={item.id} item={item} number={index + 1} names={names} checked={selected.has(item.id)} disabled={sent} onToggle={(on) => toggle(item.id, on)} />
          ))}
        </ol>
        <div className="flex flex-wrap items-center gap-1.5 pt-1">
          <Button size="sm" data-assistant-approve disabled={count === 0 || sent} onClick={() => decide({ approved: true, items: items.filter((item) => selected.has(item.id)).map((item) => item.id) })}>
            {`Approve ${count} selected`}
          </Button>
          <Button size="sm" variant="ghost" disabled={sent} onClick={() => decide({ approved: false })}>
            Decline all
          </Button>
        </div>
      </div>
    </section>
  )
}

function BatchRow({ item, number, names, checked, disabled, onToggle }: {
  item: BatchItem
  number: number
  names?: ReadonlyMap<string, string>
  checked: boolean
  disabled: boolean
  onToggle: (on: boolean) => void
}) {
  const view = describeStep(item.tool, item.args, names, item.risk)
  // with a preview, every argument stays under Details; free text always shows there in full, as plain text
  const fields = item.preview ? [] : view.fields
  const details = item.preview ? [...view.fields, ...view.details] : view.details
  const label = `${view.title}${view.subject ? `: ${view.subject}` : ''}`
  return (
    <li data-batch-item={item.id} data-checked={checked || undefined} className={cn('flex min-w-0 gap-2.5 rounded-lg border px-2.5 py-2', !checked && 'border-dashed bg-muted/30')}>
      <Checkbox className="mt-0.5" checked={checked} disabled={disabled} aria-label={`Include change ${number}: ${label}`} onCheckedChange={(on) => onToggle(on === true)} />
      {/* an unticked change fades; its checkbox stays clear, so it is easy to tick again */}
      <div className={cn('flex min-w-0 flex-1 flex-col gap-1.5 transition-opacity', !checked && 'opacity-55')}>
        <p className="flex min-w-0 items-center gap-1.5 text-xs">
          <span className="shrink-0 text-muted-foreground tabular-nums">{number}.</span>
          <span className="shrink-0 font-medium text-foreground">{view.title}</span>
          {view.subject && !item.preview ? <span className="min-w-0 truncate text-muted-foreground">{view.subject}</span> : null}
          {item.risk === 'dangerous' ? <span className="shrink-0 rounded bg-amber-500/10 px-1 text-[0.625rem] font-medium text-amber-700 dark:text-amber-400">Changes data</span> : null}
          {item.freeText ? (
            <span className="ml-auto flex shrink-0 items-center gap-0.5 text-[0.625rem] text-muted-foreground">
              Has text
              <InfoTip text="This change writes text that Orbit shows formatted elsewhere. Read it in full under Details before you approve." />
            </span>
          ) : null}
        </p>
        {item.preview ? <PreviewCard preview={item.preview} /> : null}
        {fields.length ? <Fields fields={fields} /> : null}
        {details.length ? (
          <details className="text-xs" open={item.freeText || detailsOpen(item.tool, item.args, item.preview !== null)}>
            <summary className="cursor-pointer text-muted-foreground outline-none hover:text-foreground focus-visible:text-foreground">Details</summary>
            <Fields fields={details} className="mt-1" />
          </details>
        ) : null}
      </div>
    </li>
  )
}
