// The Ask AI composer: the message field with its "/" command menu and "@" mention picker, the mentions as chips, and
// the send and stop buttons. A command only writes a prompt (or starts a new chat); a mention only names an item by its
// id, which the server reads again with the member's own access. Every label is plain text.
import { useEffect, useId, useRef, useState, type FormEvent, type KeyboardEvent, type ReactNode, type RefObject } from 'react'
import { useLocation } from 'react-router'
import { ArrowUp, DocumentText, Folder, Stop, TaskSquare, User, Xmark } from 'reicon-react'
import { cn } from 'cn'
import { Tip } from '@/components/common/Tip'
import { UserAvatar } from '@/components/common/UserAvatar'
import { Button } from '@/components/ui/button'
import { Spinner } from '@/components/ui/spinner'
import { TaskStatusIcon } from '@/features/tasks/components/TaskStatusIcon'
import {
  commandPrompt,
  insertMention,
  matchingCommands,
  MAX_MENTIONS,
  mentionQuery,
  mentionsToSend,
  parseSlash,
  removeMention,
  slashQuery,
  type Mention,
  type MentionKind,
  type SlashCommand,
} from './composer'
import { GROUP, useMentionOptions, type MentionOption } from './mentionOptions'

type Menu =
  | { type: 'slash'; items: SlashCommand[] }
  | { type: 'mention'; start: number; items: MentionOption[]; loading: boolean }

export function AssistantComposer({
  workspaceId,
  draft,
  setDraft,
  mentions,
  setMentions,
  inputRef,
  running,
  waiting,
  loading,
  onSend,
  onStop,
  onNewChat,
  newChatDisabled,
  footer,
}: {
  workspaceId: string
  draft: string
  setDraft: (text: string) => void
  mentions: Mention[]
  setMentions: (mentions: Mention[]) => void
  inputRef: RefObject<HTMLTextAreaElement | null>
  running: boolean
  waiting: boolean
  loading: boolean
  /** Sends the message; false when it could not go (a run is busy). */
  onSend: (text: string, mentions: Mention[]) => boolean
  onStop: () => void
  onNewChat: () => void
  newChatDisabled: boolean
  footer?: ReactNode
}) {
  const location = useLocation()
  const path = `${location.pathname}${location.search}`
  const [cursor, setCursor] = useState(draft.length)
  const [active, setActive] = useState(0)
  // Escape closes the menu until the text or the cursor moves again
  const [dismissed, setDismissed] = useState<string | null>(null)
  const listId = useId()
  const where = `${draft}\u0000${cursor}`

  const slash = slashQuery(draft, cursor)
  const commands = slash === null ? [] : matchingCommands(slash)
  const mentionAt = slash === null ? mentionQuery(draft, cursor, mentions.map((mention) => mention.label)) : null
  const full = mentionsToSend(draft, mentions).length >= MAX_MENTIONS
  const picker = useMentionOptions(workspaceId, mentionAt && !full ? mentionAt.query : null)
  const menu: Menu | null =
    dismissed === where
      ? null
      : commands.length > 0
        ? { type: 'slash', items: commands }
        : mentionAt && !full && (picker.options.length > 0 || picker.loading)
          ? { type: 'mention', start: mentionAt.start, items: picker.options, loading: picker.loading }
          : null
  const count = menu?.items.length ?? 0
  const highlighted = Math.max(0, Math.min(active, count - 1))
  const menuKey = menu ? `${menu.type}:${slash ?? mentionAt?.query ?? ''}` : ''
  const lastMenuKey = useRef(menuKey)
  useEffect(() => {
    // a new query highlights the first row again
    if (lastMenuKey.current !== menuKey) {
      lastMenuKey.current = menuKey
      setActive(0)
    }
  }, [menuKey])

  function place(text: string, at: number) {
    setDraft(text)
    setCursor(at)
    requestAnimationFrame(() => {
      const element = inputRef.current
      if (!element) return
      element.focus()
      element.selectionStart = at
      element.selectionEnd = at
    })
  }

  function runCommand(command: SlashCommand, rest = '') {
    if ('action' in command.run) {
      // while a run goes on there is no new chat: the draft stays as it is
      if (newChatDisabled) return
      place('', 0)
      setMentions([])
      onNewChat()
      return
    }
    const prompt = commandPrompt(command, path, rest) ?? ''
    place(prompt, prompt.length)
  }

  function pickMention(option: MentionOption, start: number) {
    const next = insertMention(draft, start, cursor, option.mention.label)
    if (!mentions.some((mention) => mention.kind === option.mention.kind && mention.id === option.mention.id)) setMentions([...mentions, option.mention])
    place(next.text, next.cursor)
  }

  function choose(index: number) {
    if (!menu) return
    if (menu.type === 'slash') runCommand(menu.items[index]!)
    else if (menu.items[index]) pickMention(menu.items[index], menu.start)
  }

  function send() {
    const text = draft.trim()
    if (!text || running || waiting || loading) return
    const slashed = parseSlash(text)
    if (slashed && 'action' in slashed.command.run) {
      runCommand(slashed.command)
      return
    }
    const message = slashed ? (commandPrompt(slashed.command, path, slashed.rest) ?? text) : text
    if (onSend(message, mentionsToSend(message, mentions))) {
      setDraft('')
      setCursor(0)
      setMentions([])
    }
  }

  function onKeyDown(event: KeyboardEvent<HTMLTextAreaElement>) {
    // the keys belong to an input method while it composes (Safari reports that only as keyCode 229)
    if (event.nativeEvent.isComposing || event.keyCode === 229) return
    if (menu) {
      if (event.key === 'Escape') {
        event.preventDefault()
        // the panel stays open: only the menu closes
        event.stopPropagation()
        setDismissed(where)
        return
      }
      if (count > 0 && (event.key === 'ArrowDown' || event.key === 'ArrowUp')) {
        event.preventDefault()
        setActive((highlighted + (event.key === 'ArrowDown' ? 1 : count - 1)) % count)
        return
      }
      if ((event.key === 'Enter' && !event.shiftKey) || (event.key === 'Tab' && count > 0)) {
        event.preventDefault()
        // still searching: wait for the rows instead of sending a half-typed "@query"
        if (count > 0) choose(highlighted)
        return
      }
    }
    if (event.key === 'Enter' && !event.shiftKey) {
      event.preventDefault()
      send()
    }
  }

  const named = mentionsToSend(draft, mentions)
  const optionId = (index: number) => `${listId}-${index}`

  return (
    <form
      className="shrink-0 px-3 pb-3"
      onSubmit={(event: FormEvent) => {
        event.preventDefault()
        send()
      }}
    >
      <div className="relative">
        {menu ? (
          <ComposerMenu id={listId} label={menu.type === 'slash' ? 'Commands' : 'Mention a task, project, doc or person'} loading={menu.type === 'mention' && menu.loading}>
            {menu.type === 'slash'
              ? menu.items.map((command, index) => (
                  <MenuRow key={command.name} id={optionId(index)} active={index === highlighted} tip={commandTip(command, path)} onHover={() => setActive(index)} onPick={() => choose(index)}>
                    <span className="shrink-0 font-mono text-[0.8125rem] font-medium text-foreground">/{command.name}</span>
                    <span className="min-w-0 truncate text-[0.8125rem] text-muted-foreground">{command.label}</span>
                  </MenuRow>
                ))
              : menu.items.map((option, index) => (
                  <div key={`${option.mention.kind}:${option.mention.id}`} role="presentation">
                    {index === 0 || menu.items[index - 1]!.mention.kind !== option.mention.kind ? (
                      <div role="presentation" className={cn('px-3 pb-0.5 text-[0.6875rem] font-semibold tracking-wider text-muted-foreground uppercase', index > 0 && 'mt-1.5')}>
                        {GROUP[option.mention.kind]}
                      </div>
                    ) : null}
                    <MenuRow id={optionId(index)} active={index === highlighted} onHover={() => setActive(index)} onPick={() => choose(index)}>
                      <MentionRow option={option} />
                    </MenuRow>
                  </div>
                ))}
            {menu.type === 'mention' && menu.items.length === 0 ? <p className="px-3 py-1 text-xs text-muted-foreground">Searching…</p> : null}
          </ComposerMenu>
        ) : null}
        <div className="flex flex-col rounded-lg border border-input bg-background transition-colors focus-within:border-ring focus-within:ring-3 focus-within:ring-ring/50 dark:bg-input/30">
          {named.length > 0 ? (
            <ul aria-label="Mentioned" className="flex flex-wrap gap-1 px-2 pt-2">
              {named.map((mention) => (
                <li key={`${mention.kind}:${mention.id}`}>
                  <MentionChip kind={mention.kind} label={mention.label}>
                    <button
                      type="button"
                      aria-label={`Remove ${mention.label}`}
                      className="-mr-0.5 ml-0.5 grid size-3.5 place-items-center rounded-sm text-muted-foreground outline-none hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring/50"
                      onClick={() => {
                        setMentions(mentions.filter((other) => other !== mention))
                        // only this mention's own "@label", not a longer one that starts the same (@ENG-1 in @ENG-12)
                        const next = removeMention(draft, mention.label)
                        place(next.text, next.cursor)
                      }}
                    >
                      <Xmark className="size-3" />
                    </button>
                  </MentionChip>
                </li>
              ))}
            </ul>
          ) : null}
          <div className="flex items-end gap-1.5 py-1 pr-1 pl-3">
            <label htmlFor="ask-ai-input" className="sr-only">
              Message Ask AI
            </label>
            <textarea
              id="ask-ai-input"
              ref={inputRef}
              rows={1}
              value={draft}
              disabled={waiting}
              role="combobox"
              aria-autocomplete="list"
              aria-expanded={menu !== null}
              aria-controls={menu ? listId : undefined}
              aria-activedescendant={menu && count > 0 ? optionId(highlighted) : undefined}
              placeholder={waiting ? 'Approve or decline the change first' : 'Ask, or type / or @…'}
              className="field-sizing-content max-h-36 min-h-8 flex-1 resize-none bg-transparent py-1.5 text-sm text-foreground outline-none placeholder:text-muted-foreground disabled:cursor-not-allowed"
              onChange={(event) => {
                setDraft(event.target.value)
                setCursor(event.target.selectionStart ?? event.target.value.length)
              }}
              onSelect={(event) => setCursor(event.currentTarget.selectionStart ?? 0)}
              onBlur={() => setDismissed(where)}
              onFocus={() => setDismissed(null)}
              onKeyDown={onKeyDown}
            />
            {running ? (
              <Tip label="Stop">
                <Button type="button" variant="secondary" size="icon-sm" aria-label="Stop" onClick={onStop}>
                  <Stop />
                </Button>
              </Tip>
            ) : (
              <Tip label="Send">
                <Button type="submit" size="icon-sm" aria-label="Send" disabled={!draft.trim() || waiting || loading}>
                  <ArrowUp />
                </Button>
              </Tip>
            )}
          </div>
        </div>
      </div>
      {footer}
    </form>
  )
}

function commandTip(command: SlashCommand, path: string): string {
  return commandPrompt(command, path) ?? 'Ends this chat and starts a new one.'
}

function ComposerMenu({ id, label, loading, children }: { id: string; label: string; loading: boolean; children: ReactNode }) {
  const list = useRef<HTMLDivElement>(null)
  useEffect(() => {
    list.current?.querySelector('[aria-selected="true"]')?.scrollIntoView?.({ block: 'nearest' })
  })
  return (
    <div className="absolute right-0 bottom-full left-0 z-50 mb-2 rounded-lg border border-border bg-popover shadow-xl">
      <div ref={list} id={id} role="listbox" aria-label={label} aria-busy={loading || undefined} className="max-h-72 overflow-y-auto overscroll-contain py-1.5">
        {children}
      </div>
      {loading ? <Spinner className="absolute top-2 right-2 size-3 text-muted-foreground" /> : null}
    </div>
  )
}

function MenuRow({ id, active, tip, onHover, onPick, children }: { id: string; active: boolean; tip?: string; onHover: () => void; onPick: () => void; children: ReactNode }) {
  const row = (
    <div
      id={id}
      role="option"
      aria-selected={active}
      data-active={active || undefined}
      className="flex min-w-0 cursor-pointer items-center gap-2 px-3 py-1.5 data-active:bg-muted"
      // the field keeps the focus
      onMouseDown={(event) => {
        event.preventDefault()
        onPick()
      }}
      onMouseEnter={onHover}
    >
      {children}
    </div>
  )
  return tip ? (
    <Tip label={tip} side="left">
      {row}
    </Tip>
  ) : (
    row
  )
}

function MentionRow({ option }: { option: MentionOption }) {
  const { mention, task, member } = option
  return (
    <>
      <span className="grid size-5 shrink-0 place-items-center">
        {task?.status ? (
          <TaskStatusIcon status={task.status} size={13} />
        ) : member ? (
          <UserAvatar user={{ name: member.name, color: member.color, avatarUrl: member.avatarUrl }} size={18} />
        ) : (
          <KindIcon kind={mention.kind} />
        )}
      </span>
      {mention.kind === 'task' ? <span className="shrink-0 text-xs font-medium text-muted-foreground tabular-nums">{mention.label}</span> : null}
      <span className="min-w-0 flex-1 truncate text-[0.8125rem] text-foreground">{option.title}</span>
      {option.detail ? <span className="max-w-[35%] shrink-0 truncate text-xs text-muted-foreground">{option.detail}</span> : null}
    </>
  )
}

export function KindIcon({ kind, className }: { kind: MentionKind; className?: string }) {
  const Icon = kind === 'task' ? TaskSquare : kind === 'project' ? Folder : kind === 'doc' ? DocumentText : User
  return <Icon className={cn('size-3.5 text-muted-foreground', className)} aria-hidden="true" />
}

const KIND_NAME: Record<MentionKind, string> = { task: 'Task', project: 'Project', doc: 'Doc', person: 'Person' }

/** A mention as a chip: its kind's icon and its label, as plain text. */
export function MentionChip({ kind, label, children, className }: { kind: MentionKind; label: string; children?: ReactNode; className?: string }) {
  return (
    <span data-mention={kind} className={cn('inline-flex max-w-full items-center gap-1 rounded-md bg-primary/10 px-1.5 py-0.5 align-baseline text-xs font-medium text-primary', className)}>
      <KindIcon kind={kind} className="size-3 text-primary" />
      <span className="sr-only">{KIND_NAME[kind]}: </span>
      <span className="min-w-0 truncate">{label}</span>
      {children}
    </span>
  )
}
