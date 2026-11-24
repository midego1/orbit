// The Ask AI panel. The conversation is kept on the server (per user and workspace, with a history of earlier ones):
// the panel loads the active one, mounts the AG-UI runtime on it and streams runs from `POST …/run`. The server stores
// a waiting change with its exact arguments; the approval card only answers approve or decline, so a reload shows the
// same card. The history (clock button) lists earlier chats; one opens read-only from its stored messages and cards,
// and Continue makes it the active chat again (its old approvals have expired on the server).
import { createContext, useContext, useEffect, useMemo, useRef, useState, type ReactNode, type Ref } from 'react'
import { Link, useLocation, useNavigate } from 'react-router'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { HttpAgent } from '@ag-ui/client'
import {
  AssistantRuntimeProvider,
  ExportedMessageRepository,
  MessagePrimitive,
  ThreadPrimitive,
  useAui,
  useAuiState,
  type ThreadHistoryAdapter,
  type ToolCallMessagePartProps,
} from '@assistant-ui/react'
import { fromAgUiMessages, useAgUiInterrupts, useAgUiRuntime } from '@assistant-ui/react-ag-ui'
import { ArrowLeft, ArrowUpRight, Clock, Copy, Edit2, Eye, EyeSlash, Maximize4, Minimize, Sparkles, TickCircle, CloseCircle, PauseCircle, Xmark } from 'reicon-react'
import { cn } from 'cn'
import { InfoTip } from '@/components/common/InfoTip'
import { Tip } from '@/components/common/Tip'
import { Button, buttonVariants } from '@/components/ui/button'
import { toast } from 'sonner'
import { Spinner } from '@/components/ui/spinner'
import { renderMarkdownBlocks } from '@/lib/markdown'
import { MarkdownTextContext } from '@/lib/markdownTextContext'
import { useWorkspace } from '@/features/workspaces/workspaceContext'
import { useAllStatuses, useProjects } from '@/features/tasks/api/projects'
import { assistantUi, useAssistantUi } from './assistantStore'
import { useFollowGuard } from './followGuard'
import { deriveRun, IDLE_RUN } from './assistantRunState'
import { AssistantStatusLine } from './AssistantRunIndicators'
import { assistantBase, assistantKeys, continueConversation, fetchAssistantStatus, modelName, openConversation, setChatModel, startConversation, stopConversation, type AssistantConversation, type AssistantStatus, type ConversationSummary } from './api'
import { AssistantHistory } from './AssistantHistory'
import { chatTitle } from './history'
import { PreviewCard, ResultCard, SummaryCard } from './AssistantCards'
import { TaskChipContext, taskChipRun } from './taskChipRun'
import { knownTasks, readCard, resultsKey, readPreview, SUMMARY_TOOL, type ApprovalPreview } from './cards'
import { AssistantComposer, MentionChip, type ChatModels } from './AssistantComposer'
import { ChatModelPicker } from './ChatModelPicker'
import { mentionSegments, readMentions, serializeMentions, suggestionsFor, type Mention } from './composer'
import { useAskDraft } from './useAskDraft'
import { appPath, describeStep, notOpened, pageTargets, parseToolResult, resultLine, stepOutcome, type StepField, type StepOutcome, type StepView } from './toolSteps'

/** True inside an earlier conversation shown read-only: no step there can answer an approval. */
const ReadOnlyContext = createContext(false)

type View = { kind: 'chat' } | { kind: 'history' } | { kind: 'earlier'; id: string }

/** Loads the user's conversation in this workspace, starts one when there is none, and mounts the runtime on it. */
export default function AssistantPanel({ onClose }: { onClose: () => void }) {
  const { workspace } = useWorkspace()
  const client = useQueryClient()
  const key = assistantKeys.status(workspace.id)
  const status = useQuery({ queryKey: key, queryFn: () => fetchAssistantStatus(workspace.id), refetchOnWindowFocus: false })
  const [startError, setStartError] = useState<string | null>(null)
  const [view, setView] = useState<View>({ kind: 'chat' })
  const [switchError, setSwitchError] = useState<string | null>(null)
  const starting = useRef(false)
  const ready = status.data?.setup === 'ready'
  const conversation = status.data?.conversation ?? null

  function showConversation(next: AssistantConversation) {
    client.setQueryData<AssistantStatus>(key, (old) => (old ? { ...old, conversation: next } : old))
    void client.invalidateQueries({ queryKey: assistantKeys.history(workspace.id) })
  }

  /** Starts a new chat; the current one moves to the history. From the history, a refusal (a run still works) shows there. */
  async function newChat(fromHistory = false, stay = false) {
    if (starting.current) return
    starting.current = true
    setStartError(null)
    setSwitchError(null)
    try {
      const started = await startConversation(workspace.id)
      showConversation(started.conversation)
      if (!stay) setView({ kind: 'chat' })
    } catch (error) {
      const message = error instanceof Error ? error.message : 'A new chat could not be started.'
      if (fromHistory) setSwitchError(message)
      else setStartError(message)
    } finally {
      starting.current = false
    }
  }

  async function continueChat(id: string) {
    setSwitchError(null)
    try {
      const resumed = await continueConversation(workspace.id, id)
      showConversation(resumed.conversation)
      setView({ kind: 'chat' })
    } catch (error) {
      setSwitchError(error instanceof Error ? error.message : 'This chat could not be continued.')
    }
  }

  function openFromHistory(summary: ConversationSummary) {
    setSwitchError(null)
    setView(summary.id === conversation?.id ? { kind: 'chat' } : { kind: 'earlier', id: summary.id })
  }

  // no conversation yet (first use, or the last one expired): start one
  useEffect(() => {
    if (ready && !conversation && !startError) void newChat()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [ready, conversation, startError])

  if (status.isPending || (ready && !conversation && !startError)) {
    return (
      <PanelShell onClose={onClose}>
        <Notice>
          <Spinner className="size-4 text-muted-foreground" />
        </Notice>
      </PanelShell>
    )
  }
  if (status.isError || startError) {
    return (
      <PanelShell onClose={onClose}>
        <Notice title="Ask AI is not available right now" text={startError ?? 'The conversation could not be loaded.'}>
          <Button
            variant="outline"
            size="sm"
            onClick={() => {
              setStartError(null)
              void status.refetch()
            }}
          >
            Try again
          </Button>
        </Notice>
      </PanelShell>
    )
  }
  if (!ready || !conversation) {
    const text =
      status.data.setup === 'no_app_key'
        ? 'The server cannot open this workspace’s API key. A workspace owner or admin can enter it again in Settings → AI.'
        : status.data.canManage
          ? 'Ask AI runs on Anthropic’s Claude models with this workspace’s own API key. Add the key in Settings → AI.'
          : 'Ask AI runs on Anthropic’s Claude models with this workspace’s own API key. A workspace owner or admin can add it in Settings → AI.'
    return (
      <PanelShell onClose={onClose}>
        <Notice title="Ask AI needs an API key" text={text}>
          {status.data.canManage ? (
            <Link to="/settings/ai" className={buttonVariants({ variant: 'outline', size: 'sm' })}>
              Open Settings → AI
            </Link>
          ) : null}
        </Notice>
      </PanelShell>
    )
  }
  const back = () => {
    setSwitchError(null)
    setView({ kind: 'chat' })
  }
  // the chat's model, as the server stored it (a pick it refuses changes nothing)
  function setModel(model: string) {
    client.setQueryData<AssistantStatus>(key, (old) => (old?.conversation ? { ...old, conversation: { ...old.conversation, model } } : old))
  }
  const chatModels: ChatModels = {
    model: conversation.model,
    models: status.data.models,
    onPick: (model) => {
      if (model === conversation.model) return
      setChatModel(workspace.id, conversation.id, model)
        .then((picked) => {
          setModel(picked.model)
          client.setQueryData<AssistantStatus>(key, (old) => (old ? { ...old, models: picked.models } : old))
          toast.success(`This chat now uses ${modelName(picked.model)}`)
        })
        .catch((error: unknown) => {
          toast.error(error instanceof Error ? error.message : 'The model could not be changed.')
          void status.refetch()
        })
    },
  }
  return (
    <>
      {/* the chat stays mounted under the history, so a run that works keeps streaming */}
      <div className={view.kind === 'chat' ? 'flex h-full min-h-0 w-full' : 'hidden'}>
        <ConversationRuntime
          key={conversation.id}
          workspaceId={workspace.id}
          conversation={conversation}
          chatModels={chatModels}
          onModelFallback={(model, previous) => {
            setModel(model)
            void status.refetch()
            toast.info(`${modelName(previous)} is no longer allowed in this workspace, so this chat now uses ${modelName(model)}.`)
          }}
          onClose={onClose}
          onNewChat={() => void newChat()}
          onHistory={() => setView({ kind: 'history' })}
          onGone={() => void status.refetch()}
        />
      </div>
      {view.kind === 'history' ? (
        <PanelShell
          onClose={onClose}
          title="History"
          leading={<BackButton onBack={back} />}
          tools={
            <Tip label="New chat">
              <Button variant="ghost" size="icon-sm" aria-label="New chat" onClick={() => void newChat(true)}>
                <Edit2 />
              </Button>
            </Tip>
          }
        >
          {switchError ? (
            <p role="alert" className="mx-3 mt-3 rounded-md bg-destructive/10 px-3 py-1.5 text-xs text-destructive">
              {switchError}
            </p>
          ) : null}
          <AssistantHistory
            workspaceId={workspace.id}
            onOpen={openFromHistory}
            onDeleted={(deleted) => {
              // the open chat is gone: a fresh one takes its place (the history stays in view)
              if (deleted.id === conversation.id) void newChat(true, true)
            }}
          />
        </PanelShell>
      ) : null}
      {view.kind === 'earlier' ? (
        <EarlierConversation
          key={view.id}
          workspaceId={workspace.id}
          id={view.id}
          onClose={onClose}
          onBack={() => setView({ kind: 'history' })}
          onContinue={() => continueChat(view.id)}
          onActive={(open) => {
            // the open chat, newer than this panel's copy: load it fresh
            if (open.id !== conversation.id) void status.refetch()
            setView({ kind: 'chat' })
          }}
          error={switchError}
        />
      ) : null}
    </>
  )
}

function BackButton({ onBack, label = 'Back to the chat' }: { onBack: () => void; label?: string }) {
  return (
    <Tip label={label}>
      <Button variant="ghost" size="icon-sm" aria-label={label} className="-ml-2" onClick={onBack}>
        <ArrowLeft />
      </Button>
    </Tip>
  )
}

/** An earlier conversation, read-only: its stored messages and the server's cards, as they were. */
function EarlierConversation({ workspaceId, id, onClose, onBack, onContinue, onActive, error }: {
  workspaceId: string
  id: string
  onClose: () => void
  onBack: () => void
  onContinue: () => Promise<void>
  /** The server says this is the open chat after all (continued in another tab): show it live. */
  onActive: (conversation: AssistantConversation) => void
  error: string | null
}) {
  const opened = useQuery({ queryKey: assistantKeys.conversation(workspaceId, id), queryFn: () => openConversation(workspaceId, id), refetchOnWindowFocus: false, gcTime: 0 })
  const active = opened.data?.conversation.active ? opened.data.conversation : null
  useEffect(() => {
    if (active) onActive(active)
    // once per answer
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [active])
  if (active) return null
  if (opened.isPending || opened.isError) {
    return (
      <PanelShell onClose={onClose} title="History" leading={<BackButton onBack={onBack} label="Back to the history" />}>
        {opened.isPending ? (
          <Notice>
            <Spinner className="size-4 text-muted-foreground" />
          </Notice>
        ) : (
          <Notice title="This chat could not be opened" text={opened.error.message}>
            <Button variant="outline" size="sm" onClick={onBack}>
              Back to the history
            </Button>
          </Notice>
        )}
      </PanelShell>
    )
  }
  return (
    <ConversationRuntime
      workspaceId={workspaceId}
      conversation={opened.data.conversation}
      onClose={onClose}
      onNewChat={() => {}}
      onGone={() => {}}
      earlier={{ onBack, onContinue, error }}
    />
  )
}

type Earlier = { onBack: () => void; onContinue: () => Promise<void>; error: string | null }

function ConversationRuntime({ workspaceId, conversation, chatModels, onModelFallback, onClose, onNewChat, onHistory, onGone, earlier }: {
  workspaceId: string
  conversation: AssistantConversation
  /** The live chat's model and the allowed ones (none for an earlier, read-only conversation). */
  chatModels?: ChatModels
  onModelFallback?: (model: string, previous: string) => void
  onClose: () => void
  onNewChat: () => void
  onHistory?: () => void
  onGone: () => void
  /** An earlier conversation, shown read-only (no runs). */
  earlier?: Earlier
}) {
  const [runError, setRunError] = useState<string | null>(null)
  // the mentions of the message being sent (kinds and ids): the server reads each item again with the member's access
  const mentions = useRef('[]')
  const agent = useMemo(() => new HttpAgent({ url: `${assistantBase(workspaceId)}/run`, threadId: conversation.id }), [workspaceId, conversation.id])
  // the server says once when an admin took this chat's model off the list
  const fallback = useRef(onModelFallback)
  useEffect(() => {
    fallback.current = onModelFallback
  })
  useEffect(() => {
    if (earlier) return
    const subscription = agent.subscribe({
      onCustomEvent: ({ event }) => {
        if (event.name !== 'orbit.model_fallback') return
        const value = event.value as { model?: unknown; previous?: unknown } | null
        if (typeof value?.model === 'string' && typeof value.previous === 'string') fallback.current?.(value.model, value.previous)
      },
    })
    return () => subscription.unsubscribe()
  }, [agent, earlier])
  // The server owns the conversation: the panel reads it once and never writes it back.
  const history = useMemo<ThreadHistoryAdapter>(
    () => ({
      load: async () => ExportedMessageRepository.fromArray(fromAgUiMessages(conversation.messages)),
      append: async () => {},
    }),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [conversation.id],
  )
  const runtime = useAgUiRuntime({
    agent,
    adapters: { history },
    // a resume continues from the server's stored state: nothing is sent back
    resumeTranscript: 'appended',
    // only the path and the Follow along switch: the server looks up what the page is itself
    getRunContext: () => {
      // a message's mentions go with its own run only (not with a later approval or message)
      const mentioned = mentions.current
      mentions.current = '[]'
      return [
        { description: 'page', value: `${window.location.pathname}${window.location.search}` },
        { description: 'followAlong', value: assistantUi.get().follow ? 'on' : 'off' },
        { description: 'mentions', value: mentioned },
      ]
    },
    onError: (error) => {
      setRunError(error.message || 'Ask AI stopped with an error.')
      if (/ended|start a new chat/i.test(error.message)) onGone()
    },
  })
  return (
    <AssistantRuntimeProvider runtime={runtime}>
      <ReadOnlyContext value={!!earlier}>
        <Thread
          chatModels={earlier ? undefined : chatModels}
          onClose={onClose}
          onNewChat={onNewChat}
          onHistory={onHistory}
          earlier={earlier ? { ...earlier, title: chatTitle(conversation) } : undefined}
          setMentions={(value) => {
            mentions.current = value
          }}
          runError={runError}
          clearError={() => setRunError(null)}
          onStop={() => void stopConversation(workspaceId, conversation.id).catch(() => {})}
        />
      </ReadOnlyContext>
    </AssistantRuntimeProvider>
  )
}

/** The panel's header and body. `tools` are the header buttons of a running conversation; `leading` (a Back button)
 * takes the place of the icon. */
function PanelShell({ onClose, title = 'Ask AI', leading, tools, children, ref }: { onClose: () => void; title?: string; leading?: ReactNode; tools?: ReactNode; children: ReactNode; ref?: Ref<HTMLElement> }) {
  return (
    <section ref={ref} aria-label={title === 'Ask AI' ? 'Ask AI' : `Ask AI: ${title}`} className="flex h-full min-h-0 w-full flex-col bg-background">
      <header className="flex min-h-12 shrink-0 items-center gap-1 border-b py-1.5 pr-2 pl-4">
        {leading ?? <Sparkles className="size-4 shrink-0 text-primary" aria-hidden="true" />}
        <h2 className="ml-1.5 min-w-0 flex-1 truncate text-[0.8125rem] font-semibold text-foreground">{title}</h2>
        {tools}
        <Tip label="Close">
          <Button variant="ghost" size="icon-sm" aria-label="Close Ask AI" onClick={onClose}>
            <Xmark />
          </Button>
        </Tip>
      </header>
      {children}
    </section>
  )
}

function Notice({ title, text, children }: { title?: string; text?: string; children?: ReactNode }) {
  return (
    <div role="status" className="flex min-h-0 flex-1 flex-col justify-end gap-3 overflow-y-auto p-4">
      {title ? <p className="text-sm font-semibold text-foreground">{title}</p> : null}
      {text ? <p className="text-[0.8125rem] text-muted-foreground">{text}</p> : null}
      {children ? <div className="flex">{children}</div> : null}
    </div>
  )
}

function isWaiting(approval: { approved?: boolean; resolution?: string } | undefined): boolean {
  return !!approval && approval.approved === undefined && !approval.resolution
}

function Thread({ chatModels, onClose, onNewChat, onHistory, earlier, setMentions: setRunMentions, runError, clearError, onStop }: {
  chatModels?: ChatModels
  onClose: () => void
  onNewChat: () => void
  onHistory?: () => void
  /** Read-only: an earlier conversation, with Continue in place of the composer. */
  earlier?: Earlier & { title: string }
  setMentions: (serialized: string) => void
  runError: string | null
  clearError: () => void
  onStop: () => void
}) {
  const navigate = useNavigate()
  const location = useLocation()
  const { workspace } = useWorkspace()
  const aui = useAui()
  const { follow, wide, focusToken } = useAssistantUi()
  const running = useAuiState((s) => s.thread.isRunning)
  // while the stored conversation loads, a sent message would be lost: the composer waits
  const loading = useAuiState((s) => s.thread.isLoading)
  const messages = useAuiState((s) => s.thread.messages)
  const [pageLink, setPageLink] = useState<string | null>(null)
  const input = useRef<HTMLTextAreaElement>(null)
  const root = useRef<HTMLElement>(null)
  const wouldInterrupt = useFollowGuard(root)
  const followRef = useRef(follow)
  useEffect(() => {
    followRef.current = follow
  }, [follow])

  // Follow along: a page step that arrives during a run moves the page there. Steps of a reloaded conversation do not.
  const seen = useRef<Set<string> | null>(null)
  useEffect(() => {
    const targets = pageTargets(messages)
    if (seen.current === null) {
      seen.current = new Set(targets.map((target) => target.id))
      setPageLink(targets.at(-1)?.path ?? null)
      return
    }
    for (const target of targets) {
      if (seen.current.has(target.id)) continue
      seen.current.add(target.id)
      setPageLink(target.path)
      if (running && followRef.current && !wouldInterrupt()) navigate(target.path)
    }
  }, [messages, running, navigate, wouldInterrupt])

  // task keys link only when this conversation's cards named them (never a key the model made up); worked out again
  // only when a tool result arrives, not with each streamed word
  const results = resultsKey(messages)
  // eslint-disable-next-line react-hooks/exhaustive-deps
  const textRun = useMemo(() => taskChipRun(knownTasks(messages)), [results])
  const waiting = messages.some((message) => message.role === 'assistant' && message.content.some((part) => part.type === 'tool-call' && isWaiting(part.approval)))
  const empty = messages.length === 0
  const last = messages.at(-1)
  const callInFlight = last?.role === 'assistant' && last.content.some((part) => part.type === 'tool-call' && part.result === undefined)
  const run = deriveRun({ running, messages })
  // not while an approved change runs: its step (in the message that asked for it) shows that, and the status line
  // says "Changing …"
  const thinking = running && !callInFlight && run.state === 'thinking'

  // an earlier conversation never runs: the page edge and status line stay with the active chat
  useEffect(() => {
    if (!earlier) assistantUi.setRun(run)
    // the run is derived from these two
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [running, messages])
  useEffect(() => () => {
    if (!earlier) assistantUi.setRun(IDLE_RUN)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])
  useEffect(() => {
    input.current?.focus()
  }, [focusToken])
  const [continuing, setContinuing] = useState(false)

  function send(text: string, mentioned: Mention[] = []): boolean {
    const message = text.trim()
    if (!message || running || waiting || loading) return false
    clearError()
    setRunMentions(serializeMentions(mentioned))
    // the message shows its mentions as chips (the server stores its own, checked list for a reload)
    const shown = mentioned.map(({ kind, label }) => ({ kind, token: label }))
    aui.thread().append({ role: 'user', content: [{ type: 'text', text: message }], metadata: { custom: shown.length ? { mentions: shown } : {} } })
    return true
  }

  // a suggestion that ends open ("Create a task: ") fills the field; the others are sent, and clear the field
  const { draft, setDraft, mentions, setMentions, suggest } = useAskDraft(send, (at) =>
    requestAnimationFrame(() => {
      const element = input.current
      if (!element) return
      element.focus()
      element.selectionStart = element.selectionEnd = at
    }),
  )
  const suggestions = suggestionsFor(`${location.pathname}${location.search}`)
  const answered = !empty && !running && !waiting && last?.role === 'assistant' && draft === ''

  const tools = earlier ? null : (
    <>
      {chatModels ? <ChatModelPicker chatModels={chatModels} disabled={running} /> : null}
      <Tip label={follow ? 'Follow along is on: the page goes where Ask AI works' : 'Follow along is off: Ask AI shows a link instead'}>
        <Button variant="ghost" size="icon-sm" aria-label="Follow along" aria-pressed={follow} className={cn(follow ? 'text-foreground' : 'text-muted-foreground')} onClick={() => assistantUi.setFollow(!follow)}>
          {follow ? <Eye /> : <EyeSlash />}
        </Button>
      </Tip>
      {onHistory ? (
        <Tip label="History">
          <Button variant="ghost" size="icon-sm" aria-label="Chat history" onClick={onHistory}>
            <Clock />
          </Button>
        </Tip>
      ) : null}
      {!empty ? (
        <Tip label="New chat">
          <Button variant="ghost" size="icon-sm" aria-label="New chat" disabled={running} onClick={onNewChat}>
            <Edit2 />
          </Button>
        </Tip>
      ) : null}
      <Tip label={wide ? 'Narrow' : 'Widen'}>
        <Button variant="ghost" size="icon-sm" aria-label={wide ? 'Narrow Ask AI' : 'Widen Ask AI'} aria-pressed={wide} className="max-[899px]:hidden" onClick={() => assistantUi.setWide(!wide)}>
          {wide ? <Minimize /> : <Maximize4 />}
        </Button>
      </Tip>
    </>
  )

  return (
    <PanelShell
      ref={root}
      onClose={onClose}
      tools={tools}
      title={earlier?.title}
      leading={earlier ? <BackButton onBack={earlier.onBack} label="Back to the history" /> : undefined}
    >
      {earlier ? null : <AssistantStatusLine />}
      <TaskChipContext value={textRun}>
        <ThreadPrimitive.Root className="flex min-h-0 flex-1 flex-col">
          <ThreadPrimitive.Viewport autoScroll className="min-h-0 flex-1 overflow-y-auto overscroll-contain px-4">
            <ThreadPrimitive.Empty>
              {/* an earlier chat is read-only: no suggestions */}
              {earlier ? null : (
                <div className="flex min-h-full flex-col justify-end gap-3 pb-4">
                  <p className="text-sm font-semibold text-foreground">What can Ask AI do for you?</p>
                  <p className="text-[0.8125rem] text-muted-foreground">Ask about your tasks, docs and chats, or tell it what to change. You approve each change before it is made.</p>
                  <div className="flex flex-col gap-1.5" aria-label="Suggestions" role="group">
                    {suggestions.map((suggestion) => (
                      <Button key={suggestion.label} variant="outline" size="sm" className="h-auto justify-start py-1.5 text-left whitespace-normal" disabled={loading} onClick={() => suggest(suggestion)}>
                        {suggestion.label}
                      </Button>
                    ))}
                  </div>
                  <p className="text-xs text-muted-foreground">Type / for commands, @ to mention a task, project, doc or person.</p>
                </div>
              )}
            </ThreadPrimitive.Empty>
            <ol className="flex flex-col gap-3 py-4" aria-live="polite">
              <ThreadPrimitive.Messages>{({ message }) => <Message role={message.role} />}</ThreadPrimitive.Messages>
              {thinking && !empty ? (
                <li className="flex items-center gap-2 text-[0.8125rem] text-muted-foreground">
                  <Spinner role="presentation" aria-hidden aria-label={undefined} className="size-3.5" />
                  Thinking…
                </li>
              ) : null}
            </ol>
          </ThreadPrimitive.Viewport>
          {earlier ? (
            <div className="flex shrink-0 flex-col gap-2 border-t px-3 py-3">
              {earlier.error ? (
                <p role="alert" className="rounded-md bg-destructive/10 px-3 py-1.5 text-xs text-destructive">
                  {earlier.error}
                </p>
              ) : null}
              <div className="flex items-center gap-2">
                <p className="flex min-w-0 flex-1 items-center gap-1.5 text-xs text-muted-foreground">
                  An earlier chat
                  <InfoTip text="Read-only. Continue it to ask more. A change it asked for that you did not approve has expired: Ask AI asks again." />
                </p>
                <Button
                  size="sm"
                  disabled={continuing}
                  onClick={() => {
                    setContinuing(true)
                    void earlier.onContinue().finally(() => setContinuing(false))
                  }}
                >
                  Continue this chat
                </Button>
              </div>
            </div>
          ) : null}
          {runError && !earlier ? (
            <p role="alert" className="mx-3 mb-2 rounded-md bg-destructive/10 px-3 py-1.5 text-xs text-destructive">
              {runError}
            </p>
          ) : null}
          {!follow && pageLink && !earlier ? (
            <button
              type="button"
              className="mx-3 mb-2 flex items-center gap-1.5 rounded-md border px-3 py-1.5 text-left text-xs text-foreground transition-colors outline-none hover:bg-muted focus-visible:ring-3 focus-visible:ring-ring/50"
              onClick={() => navigate(pageLink)}
            >
              <ArrowUpRight className="size-3.5 shrink-0 text-primary" />
              Ask AI wants to show you a page: go there
            </button>
          ) : null}
          {answered && !earlier ? (
            <div role="group" aria-label="Suggestions" className="mx-3 mb-2 flex gap-1.5 overflow-x-auto [scrollbar-width:none]">
              {suggestions.slice(0, 3).map((suggestion) => (
                <button
                  key={suggestion.label}
                  type="button"
                  className="shrink-0 rounded-full border px-2.5 py-1 text-xs whitespace-nowrap text-muted-foreground transition-colors outline-none hover:bg-muted hover:text-foreground focus-visible:ring-3 focus-visible:ring-ring/50"
                  onClick={() => suggest(suggestion)}
                >
                  {suggestion.label}
                </button>
              ))}
            </div>
          ) : null}
          {earlier ? null : (
            <AssistantComposer
              workspaceId={workspace.id}
              draft={draft}
              setDraft={setDraft}
              mentions={mentions}
              setMentions={setMentions}
              inputRef={input}
              running={running}
              waiting={waiting}
              loading={loading}
              onSend={send}
              onStop={() => {
                aui.thread().cancelRun()
                onStop()
              }}
              onNewChat={onNewChat}
              newChatDisabled={running}
              chatModels={chatModels && running ? { ...chatModels, onPick: () => toast.info('The model can change when Ask AI is done.') } : chatModels}
              footer={<p className="mt-1.5 text-center text-[0.6875rem] text-muted-foreground">Changes wait for your approval. Answers can be wrong.</p>}
            />
          )}
        </ThreadPrimitive.Root>
      </TaskChipContext>
    </PanelShell>
  )
}

function Message({ role }: { role: string }) {
  const user = role === 'user'
  return (
    <li className={cn('flex flex-col', user && 'items-end')}>
      <MessagePrimitive.Root className={cn('flex min-w-0 flex-col gap-2', user ? 'max-w-[85%] items-end' : 'w-full')}>
        <MessagePrimitive.Parts components={{ Text: user ? UserText : AnswerText, tools: { Fallback: Step } }} />
      </MessagePrimitive.Root>
    </li>
  )
}

function UserText({ text }: { text: string }) {
  const mentions = useAuiState((s) => s.message.metadata?.custom?.mentions)
  // plain text only: a mention is the user's own "@label" text, shown as a chip
  const segments = useMemo(() => mentionSegments(text, readMentions(mentions)), [text, mentions])
  return (
    <p className="rounded-lg bg-muted px-3 py-2 text-[0.8125rem] whitespace-pre-wrap text-foreground">
      {segments.map((segment, index) => (segment.mention ? <MentionChip key={index} kind={segment.mention} label={segment.text} className="mx-px" /> : <span key={index}>{segment.text}</span>))}
    </p>
  )
}

function AnswerText({ text }: { text: string }) {
  const running = useAuiState((s) => s.thread.isRunning)
  // only the message's final answer gets a copy button, not the lines between its steps
  const final = useAuiState((s) => {
    const last = s.message.parts.at(-1)
    return last?.type === 'text' && last.text === text
  })
  const [copied, setCopied] = useState(false)
  const textRun = useContext(TaskChipContext)
  async function copy() {
    try {
      await navigator.clipboard.writeText(text)
      setCopied(true)
      setTimeout(() => setCopied(false), 1500)
    } catch {
      // no clipboard outside a secure context
    }
  }
  return (
    <div className="group/answer flex min-w-0 flex-col">
      {/* answers never load images from other sites and never run diagrams (the model's text is not trusted) */}
      <div className="text-[0.8125rem] leading-relaxed break-words text-foreground [&_p]:my-1"><MarkdownTextContext value={textRun}>{renderMarkdownBlocks(text, 'ask-ai', [], { noExternalImages: true })}</MarkdownTextContext></div>
      {!running && final ? (
        <div className="-ml-1.5 flex">
          <Tip label={copied ? 'Copied' : 'Copy'}>
            <Button variant="ghost" size="icon-xs" aria-label={copied ? 'Copied' : 'Copy answer'} className="text-muted-foreground" onClick={() => void copy()}>
              {copied ? <TickCircle /> : <Copy />}
            </Button>
          </Tip>
        </div>
      ) : null}
    </div>
  )
}

/** Project and status names by id, so a card says "Project: Website" and not an id. */
function useNames(): ReadonlyMap<string, string> {
  const { workspace } = useWorkspace()
  const projects = useProjects(workspace.id)
  const statuses = useAllStatuses(workspace.id, projects.data ?? [])
  return useMemo(() => {
    const names = new Map<string, string>()
    for (const project of projects.data ?? []) names.set(project.id, project.name)
    for (const status of statuses.data) names.set(status.id, status.name)
    return names
  }, [projects.data, statuses.data])
}

const OUTCOME_LABEL: Record<StepOutcome, string> = {
  running: 'Working',
  waiting: 'Waiting for your approval',
  done: 'Done',
  failed: 'Failed',
  declined: 'Declined',
  skipped: 'Not done',
}

function OutcomeIcon({ outcome }: { outcome: StepOutcome }) {
  if (outcome === 'running') return <Spinner role="presentation" aria-hidden aria-label={undefined} className="size-3.5 text-muted-foreground" />
  if (outcome === 'done') return <TickCircle className="size-3.5 text-primary" aria-hidden="true" />
  if (outcome === 'failed') return <CloseCircle className="size-3.5 text-destructive" aria-hidden="true" />
  return <PauseCircle className="size-3.5 text-muted-foreground" aria-hidden="true" />
}

/** One tool call: a step row, or the approval card while the call waits for the user. */
function Step(props: ToolCallMessagePartProps) {
  const { toolName, args, respondToApproval } = props
  const navigate = useNavigate()
  const names = useNames()
  const result = parseToolResult(props.result)
  const outcome = stepOutcome({ approval: props.approval, result, isError: props.isError, status: props.status })
  // an earlier chat is read-only: a call that still waits shows as a step, without Approve or Decline
  const readOnly = useContext(ReadOnlyContext)
  // the server's interrupt says how risky the change is and whether "Allow in this chat" may be offered
  const interrupt = useAgUiInterrupts().find((entry) => entry.toolCallId === props.toolCallId)
  const view = describeStep(toolName, args, names, interrupt?.metadata?.risk)
  // the server's card of this result (or the summary at the end of a message), never built from the model's text
  const card = outcome === 'done' ? readCard(result, toolName) : null
  if (toolName === SUMMARY_TOOL) return card?.kind === 'summary' ? <SummaryCard card={card} /> : null
  if (outcome === 'waiting' && !readOnly) {
    return (
      <ApprovalCard
        view={view}
        preview={readPreview(interrupt?.metadata?.preview)}
        allowable={interrupt?.metadata?.allowForConversation === true && view.risk === 'write'}
        onDecide={(approved, forChat) => void respondToApproval(forChat ? { approved, reason: 'allow_for_conversation' } : { approved })}
      />
    )
  }
  const linkOnly = outcome === 'done' && notOpened(toolName, result)
  // only a link the server sent back opens a page, never one from the call's own arguments
  const open = outcome === 'done' && result !== null && typeof result === 'object' ? appPath((result as { url?: unknown }).url) : null
  const line = outcome === 'declined' ? '' : card?.kind === 'tasks' ? (card.totals.tasks === 1 ? '1 result' : `${card.totals.tasks} results`) : resultLine(result)
  const note = line === view.subject || linkOnly ? '' : line
  const isChange = view.risk !== 'read'
  return (
    // the step that runs now gets a soft ring in the state's colour (index.css)
    <div data-assistant-step={outcome === 'running' ? 'running' : undefined} className="flex min-w-0 flex-col">
      <div className="flex min-w-0 items-center gap-2 text-[0.8125rem]">
        <span className="flex size-4 shrink-0 items-center justify-center">
          {linkOnly ? <ArrowUpRight className="size-3.5 text-muted-foreground" aria-hidden="true" /> : <OutcomeIcon outcome={outcome} />}
          <span className="sr-only">{linkOnly ? 'Not opened' : OUTCOME_LABEL[outcome]}</span>
        </span>
        <span className={cn('shrink-0', outcome === 'running' ? 'text-muted-foreground' : 'text-foreground')}>{linkOnly ? 'Page not opened (Follow along is off)' : view.title}</span>
        {view.subject && !linkOnly ? <span className="min-w-0 truncate text-muted-foreground">{view.subject}</span> : null}
        {note && outcome === 'done' ? <span className="ml-auto shrink-0 text-xs text-muted-foreground">{note}</span> : null}
        {open ? (
          <Tip label="Open">
            <Button variant="ghost" size="icon-xs" aria-label="Open" className={cn('text-muted-foreground', !(note && outcome === 'done') && 'ml-auto')} onClick={() => navigate(open)}>
              <ArrowUpRight />
            </Button>
          </Tip>
        ) : null}
      </div>
      {outcome === 'declined' ? <p className="ml-6 text-xs text-muted-foreground">Declined</p> : note && outcome !== 'done' ? <p className={cn('ml-6 text-xs', outcome === 'failed' ? 'text-destructive' : 'text-muted-foreground')}>{note}</p> : null}
      {card ? (
        <div className="mt-1.5 ml-6">
          <ResultCard card={card} />
        </div>
      ) : null}
      {/* a change keeps what it was, folded, so the record stays complete */}
      {isChange && outcome !== 'running' && view.fields.length + view.details.length > 0 ? (
        <details className="mt-1 ml-6 text-xs">
          <summary className="cursor-pointer text-muted-foreground outline-none hover:text-foreground focus-visible:text-foreground">Details</summary>
          <Fields fields={[...view.fields, ...view.details]} className="mt-1" />
        </details>
      ) : null}
    </div>
  )
}

function Fields({ fields, className }: { fields: StepField[]; className?: string }) {
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

function ApprovalCard({ view, preview, allowable, onDecide }: { view: StepView; preview: ApprovalPreview | null; allowable: boolean; onDecide: (approved: boolean, forChat?: boolean) => void }) {
  // with a preview, the full arguments stay under Details
  const fields = preview ? [] : view.fields
  const details = preview ? [...view.fields, ...view.details] : view.details
  const dangerous = view.risk === 'dangerous'
  return (
    <section data-assistant-approval aria-label={`Approve: ${view.title}`} className="flex flex-col rounded-lg bg-card shadow-[0_0_0_1px_var(--border)]">
      <header className="flex flex-col gap-0.5 px-3 pt-2.5 pb-2">
        <p className="text-[0.8125rem] font-medium text-foreground">{view.title}</p>
        <p className={cn('text-xs', dangerous ? 'text-amber-700 dark:text-amber-400' : 'text-muted-foreground')}>
          {dangerous ? 'This changes existing data. Check it before you approve.' : 'Ask AI wants to make this change.'}
        </p>
      </header>
      <div className="flex flex-col gap-1.5 rounded-b-lg border-t bg-background px-3 py-2.5">
        {preview ? <PreviewCard preview={preview} /> : null}
        {fields.length ? <Fields fields={fields} /> : null}
        {details.length ? (
          <details className="text-xs">
            <summary className="cursor-pointer text-muted-foreground outline-none hover:text-foreground focus-visible:text-foreground">Details</summary>
            <Fields fields={details} className="mt-1" />
          </details>
        ) : null}
        <div className="flex flex-wrap items-center gap-1.5 pt-1">
          <Button size="sm" data-assistant-approve onClick={() => onDecide(true)}>
            Approve
          </Button>
          {allowable ? (
            <Tip label="Approve this, and let Ask AI make this kind of change without asking again in this chat">
              <Button size="sm" variant="outline" data-assistant-allow onClick={() => onDecide(true, true)}>
                Allow in this chat
              </Button>
            </Tip>
          ) : null}
          <Button size="sm" variant="ghost" onClick={() => onDecide(false)}>
            Decline
          </Button>
        </div>
      </div>
    </section>
  )
}
