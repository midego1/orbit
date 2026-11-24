// The cards Ask AI shows for tool results (`cards.ts`). Every text in them is plain text in a React text node, never
// Markdown or HTML, and every link is a checked in-app path (`cardPath`).
import { useContext, useState, type ReactNode } from 'react'
import { Link } from 'react-router'
import { ArrowRight, CloseCircle, DocumentText, Folder2, MinusCircle, TickCircle, Undo } from 'reicon-react'
import { cn } from 'cn'
import { Tip } from '@/components/common/Tip'
import { Button } from '@/components/ui/button'
import { Spinner } from '@/components/ui/spinner'
import { UserAvatar } from '@/components/common/UserAvatar'
import { relativeTime } from '@/lib/format'
import { PriorityIcon } from '@/features/tasks/components/PriorityIcon'
import { TaskStatusIcon } from '@/features/tasks/components/TaskStatusIcon'
import { PRIORITY_LABEL } from '@/features/tasks/taskMeta'
import { groupTasks, outcomeLine, totalsLine, isDone, type ApprovalPreview, type CardData, type CardStatus, type Change, type KnownTask, type LinkItem, type SummaryCardData, type TaskCardData, type TasksCardData, type UndoState } from './cards'
import { UndoContext } from './undoContext'
import type { StepField } from './toolSteps'

/** Rows shown before "Show all". */
const LIST_PREVIEW = 5
const NEUTRAL = '#94a3b8'

const linkRing = 'outline-none focus-visible:ring-3 focus-visible:ring-ring/50'

function StatusGlyph({ status, size = 14 }: { status: CardStatus | null; size?: number }) {
  return <TaskStatusIcon status={status ? { category: status.category, color: status.color ?? NEUTRAL } : undefined} size={size} />
}

function StatusPill({ status }: { status: CardStatus | null }) {
  if (!status) return null
  return (
    <span data-slot="status-pill" className="inline-flex max-w-40 shrink-0 items-center gap-1 rounded-full border px-1.5 py-px text-[0.6875rem] text-foreground">
      <StatusGlyph status={status} size={12} />
      <span className="truncate">{status.name}</span>
    </span>
  )
}

function dueLabel(due: string): string {
  const date = new Date(`${due}T00:00:00`)
  return Number.isNaN(date.getTime()) ? due : date.toLocaleDateString(undefined, { day: 'numeric', month: 'short' })
}

function Assignees({ people }: { people: { name: string }[] }) {
  if (people.length === 0) return null
  const names = people.map((person) => person.name).join(', ')
  return (
    <Tip label={names}>
      <span className="flex shrink-0 -space-x-1" aria-label={`Assigned to ${names}`}>
        {people.slice(0, 3).map((person, index) => (
          <UserAvatar key={index} user={{ name: person.name, color: NEUTRAL }} size={18} className="ring-2 ring-card" />
        ))}
      </span>
    </Tip>
  )
}

/** One task: key, title, status, priority, assignees and due date. The whole card links to the task when it exists. */
export function TaskCard({ task, preview }: { task: TaskCardData; preview?: boolean }) {
  const body = (
    <>
      <span className="flex min-w-0 items-center gap-1.5">
        <StatusGlyph status={task.status} />
        {task.key ? <span className="shrink-0 font-mono text-[0.6875rem] text-muted-foreground">{task.key}</span> : null}
        {preview ? <span className="shrink-0 rounded bg-primary/10 px-1 text-[0.625rem] font-medium tracking-wide text-primary uppercase">New</span> : null}
        <span className={cn('min-w-0 flex-1 truncate font-medium text-foreground', isDone(task) && 'text-muted-foreground line-through decoration-muted-foreground/60')}>{task.title || 'Untitled'}</span>
      </span>
      <span className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1 text-[0.6875rem] text-muted-foreground">
        <StatusPill status={task.status} />
        <span className="inline-flex items-center gap-1">
          <PriorityIcon priority={task.priority} size={12} />
          {PRIORITY_LABEL[task.priority]}
        </span>
        {task.due ? <span>Due {dueLabel(task.due)}</span> : null}
        {task.project ? <span className="truncate">{task.project.name}</span> : null}
        <span className="ml-auto">
          <Assignees people={task.assignees} />
        </span>
      </span>
    </>
  )
  const className = 'flex min-w-0 flex-col gap-1.5 rounded-lg border bg-card px-3 py-2 text-[0.8125rem]'
  return task.href ? (
    <Link to={task.href} data-slot="task-card" className={cn(className, linkRing, 'transition-colors hover:bg-muted/60')}>
      {body}
    </Link>
  ) : (
    <div data-slot="task-card" className={className}>
      {body}
    </div>
  )
}

function TaskRow({ task }: { task: TaskCardData }) {
  return (
    <li>
      <Link to={task.href ?? '#'} data-slot="task-row" className={cn('flex min-w-0 items-center gap-2 rounded-md px-1.5 py-1 text-xs transition-colors hover:bg-muted', linkRing)}>
        <StatusGlyph status={task.status} size={13} />
        {task.key ? <span className="w-14 shrink-0 truncate font-mono text-[0.6875rem] text-muted-foreground">{task.key}</span> : null}
        <span className={cn('min-w-0 flex-1 truncate text-foreground', isDone(task) && 'text-muted-foreground line-through decoration-muted-foreground/60')}>{task.title || 'Untitled'}</span>
        <Tip label={PRIORITY_LABEL[task.priority]}>
          <span className="shrink-0">
            <PriorityIcon priority={task.priority} size={12} />
          </span>
        </Tip>
        {task.due ? <span className="shrink-0 text-[0.6875rem] text-muted-foreground tabular-nums">{dueLabel(task.due)}</span> : null}
        <Assignees people={task.assignees.slice(0, 1)} />
      </Link>
    </li>
  )
}

/** A task list: exact counts from the server, rows grouped by project (or status), folded above five rows. */
export function TaskListCard({ card }: { card: TasksCardData }) {
  const [all, setAll] = useState(card.items.length <= LIST_PREVIEW)
  const limit = all ? card.items.length : LIST_PREVIEW
  const listedAll = card.items.length === card.totals.tasks
  const groups = groupTasks(card.items, card.totals)
    .map((group, index, every) => {
      const before = every.slice(0, index).reduce((sum, earlier) => sum + earlier.tasks.length, 0)
      return { ...group, shown: group.tasks.slice(0, Math.max(0, limit - before)) }
    })
    .filter((group) => group.shown.length > 0)
  return (
    <section data-slot="task-list-card" aria-label="Tasks" className="flex flex-col gap-1 rounded-lg border bg-card px-2 py-2">
      <p className="px-1.5 text-[0.6875rem] text-muted-foreground">
        {totalsLine(card)}
      </p>
      {groups.map((group) => (
        <div key={group.label} className="flex flex-col">
          <p className="flex items-center gap-1.5 px-1.5 pt-1.5 pb-0.5 text-[0.6875rem] font-medium text-foreground">
            {group.label}
            {/* "12 of 30": the card lists fewer of this group than the server counted */}
            <span className="text-muted-foreground tabular-nums">{group.count > group.tasks.length ? `${group.tasks.length} of ${group.count}` : group.count}</span>
          </p>
          <ul className="flex flex-col">
            {group.shown.map((task) => (
              <TaskRow key={task.href ?? task.key ?? task.title} task={task} />
            ))}
          </ul>
        </div>
      ))}
      {card.items.length > LIST_PREVIEW ? (
        <button type="button" aria-expanded={all} className={cn('mx-1.5 mt-0.5 self-start rounded text-xs text-primary hover:underline', linkRing)} onClick={() => setAll(!all)}>
          {all ? 'Show less' : listedAll ? `Show all ${card.items.length}` : `Show the ${card.items.length} listed`}
        </button>
      ) : null}
    </section>
  )
}

function LinkList({ items, icon, label }: { items: LinkItem[]; icon: ReactNode; label: string }) {
  const [all, setAll] = useState(items.length <= LIST_PREVIEW)
  return (
    <section aria-label={label} className="flex flex-col rounded-lg border bg-card px-2 py-1.5">
      <ul className="flex flex-col">
        {(all ? items : items.slice(0, LIST_PREVIEW)).map((item) => (
          <li key={item.href}>
            <Link to={item.href} className={cn('flex min-w-0 items-center gap-2 rounded-md px-1.5 py-1 text-xs transition-colors hover:bg-muted', linkRing)}>
              <span className="shrink-0 text-muted-foreground">{icon}</span>
              <span className="min-w-0 flex-1 truncate text-foreground">{item.title}</span>
              {item.key ? <span className="shrink-0 font-mono text-[0.6875rem] text-muted-foreground">{item.key}</span> : null}
              {item.updatedAt ? <span className="shrink-0 text-[0.6875rem] text-muted-foreground">{relativeTime(item.updatedAt)}</span> : null}
            </Link>
          </li>
        ))}
      </ul>
      {items.length > LIST_PREVIEW ? (
        <button type="button" aria-expanded={all} className={cn('mx-1.5 my-0.5 self-start rounded text-xs text-primary hover:underline', linkRing)} onClick={() => setAll(!all)}>
          {all ? 'Show less' : `Show all ${items.length}`}
        </button>
      ) : null}
    </section>
  )
}

type UndoView = { state: UndoState; message: string }

/** The undo state the server answered, as the row shows it. */
function undoView(outcome: string, message: string): UndoView {
  switch (outcome) {
    case 'undone':
    case 'already_undone':
      return { state: 'undone', message: outcome === 'undone' ? message : '' }
    case 'conflict':
    case 'uncertain':
      return { state: 'conflict', message }
    case 'expired':
    case 'not_found':
      return { state: 'expired', message }
    default:
      return { state: 'available', message }
  }
}

/** The end of a message that made changes: one row per change (a batch also lists what failed or did not run), each
    applied task change with Undo while the server allows it, and Undo all. */
export function SummaryCard({ card }: { card: SummaryCardData }) {
  const undo = useContext(UndoContext)
  const [views, setViews] = useState<Record<string, UndoView>>({})
  const view = (change: Change): UndoView | null => (change.undo ? (views[change.undo.id] ?? { state: change.undo.state, message: '' }) : null)
  const applied = card.changes.filter((change) => change.outcome === 'applied').length
  const failed = card.changes.filter((change) => change.outcome === 'failed').length
  const skipped = card.changes.filter((change) => change.outcome === 'skipped').length
  const undoable = card.changes.flatMap((change) => (change.undo && view(change)?.state === 'available' ? [change.undo.id] : []))
  const busy = card.changes.some((change) => view(change)?.state === 'undoing')

  async function run(ids: string[]) {
    if (!undo || ids.length === 0) return
    setViews((old) => ({ ...old, ...Object.fromEntries(ids.map((id) => [id, { state: 'undoing' as const, message: '' }])) }))
    try {
      const results = await undo(ids)
      setViews((old) => {
        const next = { ...old }
        for (const id of ids) {
          const result = results.find((entry) => entry.id === id)
          next[id] = result ? undoView(result.outcome, result.message) : { state: 'available', message: 'Not undone. Try again.' }
        }
        return next
      })
    } catch (error) {
      const message = error instanceof Error ? error.message : 'Not undone. Try again.'
      setViews((old) => ({ ...old, ...Object.fromEntries(ids.map((id) => [id, { state: 'available' as const, message }])) }))
    }
  }

  const heading = failed || skipped ? outcomeLine({ applied, failed, skipped }) : applied === 1 ? '1 change applied' : `${applied} changes applied`
  return (
    <section data-slot="summary-card" aria-label="Changes applied" className="flex flex-col gap-1 rounded-lg border bg-card px-3 py-2">
      <div className="flex min-w-0 items-center gap-1.5">
        {failed ? <CloseCircle className="size-4 shrink-0 text-destructive" aria-hidden="true" /> : <TickCircle className="size-4 shrink-0 text-primary" aria-hidden="true" />}
        <p className="min-w-0 flex-1 text-[0.8125rem] font-medium text-foreground">{heading}</p>
        {undo && undoable.length > 1 ? (
          <Button size="xs" variant="outline" data-assistant-undo-all disabled={busy} onClick={() => void run(undoable)}>
            <Undo />
            Undo all
          </Button>
        ) : null}
      </div>
      <ul className="flex flex-col">
        {card.changes.map((change, index) => (
          <SummaryRow key={index} change={change} view={view(change)} canUndo={!!undo && !busy} onUndo={(id) => void run([id])} />
        ))}
      </ul>
    </section>
  )
}

function SummaryRow({ change, view, canUndo, onUndo }: { change: Change; view: UndoView | null; canUndo: boolean; onUndo: (id: string) => void }) {
  const applied = change.outcome === 'applied'
  const undone = view?.state === 'undone'
  const content = (
    <>
      {change.outcome === 'failed' ? <CloseCircle className="size-3.5 shrink-0 text-destructive" aria-hidden="true" /> : change.outcome === 'skipped' ? <MinusCircle className="size-3.5 shrink-0 text-muted-foreground" aria-hidden="true" /> : null}
      <span className="shrink-0 text-muted-foreground">{change.verb}</span>
      {change.key ? <span className="shrink-0 font-mono text-[0.6875rem] text-muted-foreground">{change.key}</span> : null}
      <span className={cn('min-w-0 flex-1 truncate', applied ? 'text-foreground' : 'text-muted-foreground', undone && 'line-through decoration-muted-foreground/60')}>{change.title}</span>
      {applied && change.href && !(undone && change.verb === 'Created') ? <ArrowRight className="size-3.5 shrink-0 text-muted-foreground" aria-hidden="true" /> : null}
    </>
  )
  const label = change.key ?? change.title
  // a created task that was undone is in the trash: its link would lead nowhere
  const href = applied && !(undone && change.verb === 'Created') ? change.href : null
  return (
    <li data-change-outcome={change.outcome} className="flex min-w-0 flex-col">
      <div className="flex min-w-0 items-center gap-1">
        {href ? (
          <Link to={href} className={cn('flex min-w-0 flex-1 items-center gap-2 rounded-md px-1 py-0.5 text-xs hover:bg-muted', linkRing)}>
            {content}
          </Link>
        ) : (
          <span className="flex min-w-0 flex-1 items-center gap-2 px-1 py-0.5 text-xs">{content}</span>
        )}
        {change.undo && view ? (
          view.state === 'available' && canUndo ? (
            <Button size="xs" variant="ghost" data-assistant-undo aria-label={`Undo: ${change.verb} ${label}`} className="shrink-0 text-muted-foreground" onClick={() => onUndo(change.undo!.id)}>
              Undo
            </Button>
          ) : view.state === 'undoing' ? (
            <span className="flex shrink-0 items-center gap-1 px-1 text-[0.6875rem] text-muted-foreground">
              <Spinner role="presentation" aria-hidden aria-label={undefined} className="size-3" />
              Undoing…
            </span>
          ) : view.state === 'undone' ? (
            <span className="shrink-0 px-1 text-[0.6875rem] text-muted-foreground">Undone</span>
          ) : view.state === 'expired' ? (
            <Tip label="Changes can be undone for 24 hours, in the same chat.">
              <span className="shrink-0 px-1 text-[0.6875rem] text-muted-foreground">Undo expired</span>
            </Tip>
          ) : view.state === 'conflict' ? (
            <span className="shrink-0 px-1 text-[0.6875rem] text-muted-foreground">Can’t undo</span>
          ) : null
        ) : null}
      </div>
      {change.note && !applied ? <p className="ml-6 text-[0.6875rem] break-words text-muted-foreground">{change.outcome === 'failed' ? `Failed: ${change.note}` : `Not run: ${change.note}`}</p> : null}
      {view?.message && view.state !== 'undone' ? (
        <p role="alert" className={cn('ml-1 text-[0.6875rem] break-words', view.state === 'conflict' ? 'text-amber-700 dark:text-amber-400' : 'text-destructive')}>
          {view.message}
        </p>
      ) : null}
    </li>
  )
}

/** The card of a step's result, below the step. */
export function ResultCard({ card }: { card: CardData }) {
  switch (card.kind) {
    case 'task':
      return <TaskCard task={card} />
    case 'tasks':
      return card.items.length ? <TaskListCard card={card} /> : null
    case 'docs':
      return <LinkList items={card.items} label="Docs" icon={<DocumentText className="size-3.5" />} />
    case 'projects':
      return <LinkList items={card.items} label="Projects" icon={<Folder2 className="size-3.5" />} />
    case 'summary':
      return <SummaryCard card={card} />
  }
}

/** What an approval would do, from the server's reading of the item: before → after, or the new task. */
export function PreviewCard({ preview }: { preview: ApprovalPreview }) {
  if (preview.kind === 'create') return <TaskCard task={preview.task} preview />
  return (
    <div className="flex flex-col gap-1.5">
      <TaskCard task={preview.task} />
      {preview.changes.length ? (
        <dl data-slot="change-list" className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-1 rounded-md bg-muted/60 px-3 py-2 text-xs">
          {preview.changes.map((change) => (
            <div key={change.field} className="contents">
              <dt className="text-muted-foreground">{change.field}</dt>
              <dd className="flex min-w-0 flex-wrap items-center gap-1.5 break-words">
                <del className="text-muted-foreground decoration-muted-foreground/70">
                  <span className="sr-only">was </span>
                  {change.before}
                </del>
                <ArrowRight className="size-3 shrink-0 text-muted-foreground" aria-hidden="true" />
                <ins className="font-medium text-foreground no-underline">
                  <span className="sr-only">becomes </span>
                  {change.after}
                </ins>
              </dd>
            </div>
          ))}
        </dl>
      ) : null}
    </div>
  )
}

/** A task key in an answer: a link to the task, with its title and status on hover and focus. */
export function TaskChip({ task, text }: { task: KnownTask; text: string }) {
  return (
    <Tip
      label={
        <span className="flex max-w-64 flex-col gap-0.5">
          <span className="font-medium">{task.title || 'Untitled'}</span>
          {task.status ? <span className="opacity-80">{task.status.name}</span> : null}
        </span>
      }
    >
      <Link
        to={task.href}
        data-slot="task-chip"
        className={cn('inline-flex items-center gap-1 rounded border bg-muted/50 px-1 py-px align-baseline font-mono text-[0.75rem] leading-tight text-foreground no-underline transition-colors hover:bg-muted', linkRing)}
      >
        <StatusGlyph status={task.status} size={11} />
        {text}
      </Link>
    </Tip>
  )
}


/** A change's arguments: plain text, never rendered (fences and HTML stay visible). */
export function Fields({ fields, className }: { fields: StepField[]; className?: string }) {
  return (
    <dl className={cn('grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-1 rounded-md bg-muted/60 px-3 py-2 text-xs', className)}>
      {fields.map((field) => (
        <div key={field.label} className="contents">
          <dt className="text-muted-foreground">{field.label}</dt>
          {/* plain text, never rendered: fences and HTML stay visible */}
          <dd className="max-h-60 min-w-0 overflow-y-auto font-mono break-words whitespace-pre-wrap text-foreground">{field.value}</dd>
        </div>
      ))}
    </dl>
  )
}
