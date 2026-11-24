// Ask AI's history: the member's earlier conversations in this workspace (the server keeps them 30 days, for that
// member only), newest use first. Search filters the loaded titles; a row opens its conversation; rename and delete
// sit next to it. Titles are plain text.
import { useMemo, useState, type FormEvent, type KeyboardEvent } from 'react'
import { useInfiniteQuery, useQueryClient, type InfiniteData } from '@tanstack/react-query'
import { Edit, SearchNormal, Trash } from 'reicon-react'
import { cn } from 'cn'
import { Tip } from '@/components/common/Tip'
import { AlertDialog, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle } from '@/components/ui/alert-dialog'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Spinner } from '@/components/ui/spinner'
import { relativeTime } from '@/lib/format'
import { assistantKeys, deleteConversation, fetchConversations, renameConversation, type ConversationPage, type ConversationSummary } from './api'
import { chatTitle, filterConversations, UNTITLED } from './history'

const ROW_ACTION = 'opacity-0 group-hover/row:opacity-100 group-focus-within/row:opacity-100 focus-visible:opacity-100 [@media(hover:none)]:opacity-100'

type Pages = InfiniteData<ConversationPage, string | null>

export function AssistantHistory({ workspaceId, onOpen, onDeleted }: {
  workspaceId: string
  /** Opens a conversation (the active one goes back to the chat). */
  onOpen: (conversation: ConversationSummary) => void
  /** After a delete; the panel starts a new chat when it was the active one. */
  onDeleted: (conversation: ConversationSummary) => void
}) {
  const client = useQueryClient()
  const key = assistantKeys.history(workspaceId)
  const history = useInfiniteQuery({
    queryKey: key,
    queryFn: ({ pageParam }) => fetchConversations(workspaceId, pageParam),
    initialPageParam: null as string | null,
    getNextPageParam: (page) => page.nextCursor,
    refetchOnWindowFocus: false,
  })
  const [filter, setFilter] = useState('')
  const [renaming, setRenaming] = useState<string | null>(null)
  const [deleting, setDeleting] = useState<ConversationSummary | null>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const conversations = useMemo(() => history.data?.pages.flatMap((page) => page.conversations) ?? [], [history.data])
  const shown = filterConversations(conversations, filter)

  function patch(update: (rows: ConversationSummary[]) => ConversationSummary[]) {
    client.setQueryData<Pages>(key, (old) => (old ? { ...old, pages: old.pages.map((page) => ({ ...page, conversations: update(page.conversations) })) } : old))
  }

  async function rename(conversation: ConversationSummary, title: string) {
    setRenaming(null)
    if (!title.trim() || title.trim() === chatTitle(conversation)) return
    setError(null)
    try {
      const renamed = await renameConversation(workspaceId, conversation.id, title)
      patch((rows) => rows.map((row) => (row.id === conversation.id ? { ...row, title: renamed.conversation.title } : row)))
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : 'The chat could not be renamed.')
    }
  }

  async function remove(conversation: ConversationSummary) {
    setBusy(true)
    setError(null)
    try {
      await deleteConversation(workspaceId, conversation.id)
      patch((rows) => rows.filter((row) => row.id !== conversation.id))
      setDeleting(null)
      onDeleted(conversation)
    } catch (failure) {
      setDeleting(null)
      setError(failure instanceof Error ? failure.message : 'The chat could not be deleted.')
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="shrink-0 px-3 pt-3 pb-2">
        <div className="relative">
          <SearchNormal className="pointer-events-none absolute top-1/2 left-2.5 size-3.5 -translate-y-1/2 text-muted-foreground" aria-hidden="true" />
          <Input
            type="search"
            aria-label="Search chats"
            placeholder="Search chats"
            autoFocus
            value={filter}
            className="pl-8"
            onChange={(event) => setFilter(event.target.value)}
          />
        </div>
      </div>
      {error ? (
        <p role="alert" className="mx-3 mb-2 rounded-md bg-destructive/10 px-3 py-1.5 text-xs text-destructive">
          {error}
        </p>
      ) : null}
      <div className="min-h-0 flex-1 overflow-y-auto overscroll-contain px-2 pb-3">
        {history.isPending ? (
          <div role="status" aria-label="Loading chats" className="flex justify-center py-6">
            <Spinner className="size-4 text-muted-foreground" />
          </div>
        ) : history.isError ? (
          <div role="alert" className="flex flex-col items-start gap-2 px-2 py-4">
            <p className="text-[0.8125rem] text-muted-foreground">The chats could not be loaded.</p>
            <Button variant="outline" size="sm" onClick={() => void history.refetch()}>
              Try again
            </Button>
          </div>
        ) : conversations.length === 0 ? (
          <div className="flex flex-col gap-1 px-2 py-6">
            <p className="text-sm font-semibold text-foreground">No earlier chats yet</p>
            <p className="text-[0.8125rem] text-muted-foreground">Your chats with Ask AI show here for 30 days. Only you can see them.</p>
          </div>
        ) : shown.length === 0 ? (
          <p className="px-2 py-6 text-[0.8125rem] text-muted-foreground">No chats match “{filter.trim()}”.</p>
        ) : (
          <ul aria-label="Earlier chats" className="flex flex-col gap-0.5">
            {shown.map((conversation) =>
              renaming === conversation.id ? (
                <RenameRow key={conversation.id} conversation={conversation} onDone={(title) => void rename(conversation, title)} onCancel={() => setRenaming(null)} />
              ) : (
                <HistoryRow
                  key={conversation.id}
                  conversation={conversation}
                  onOpen={() => onOpen(conversation)}
                  onRename={() => setRenaming(conversation.id)}
                  onDelete={() => setDeleting(conversation)}
                />
              ),
            )}
          </ul>
        )}
        {history.hasNextPage && filter.trim() ? (
          <p className="px-2 pt-3 text-xs text-muted-foreground">Search covers the chats loaded so far. Load older chats to search them too.</p>
        ) : null}
        {history.hasNextPage ? (
          <div className="flex justify-center pt-2">
            <Button variant="ghost" size="sm" disabled={history.isFetchingNextPage} onClick={() => void history.fetchNextPage()}>
              {history.isFetchingNextPage ? 'Loading…' : 'Load older chats'}
            </Button>
          </div>
        ) : null}
      </div>
      <AlertDialog open={deleting !== null} onOpenChange={(open) => (open || busy ? null : setDeleting(null))}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete this chat?</AlertDialogTitle>
            <AlertDialogDescription>
              {/* plain text: the title is the member's own message */}
              “{deleting ? chatTitle(deleting) : ''}” and its messages are deleted for good. Changes Ask AI made in it stay.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <Button type="button" variant="outline" disabled={busy} onClick={() => setDeleting(null)}>
              Cancel
            </Button>
            <Button type="button" variant="destructive" disabled={busy} onClick={() => deleting && void remove(deleting)}>
              Delete
            </Button>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  )
}

function HistoryRow({ conversation, onOpen, onRename, onDelete }: {
  conversation: ConversationSummary
  onOpen: () => void
  onRename: () => void
  onDelete: () => void
}) {
  const title = chatTitle(conversation)
  const updated = new Date(conversation.updatedAt).toISOString()
  return (
    <li className={cn('group/row flex min-w-0 items-center gap-0.5 rounded-md pr-1 transition-colors hover:bg-muted focus-within:bg-muted', conversation.active && 'bg-muted/60')}>
      <button
        type="button"
        aria-current={conversation.active ? 'true' : undefined}
        className="flex min-w-0 flex-1 flex-col items-start gap-0.5 rounded-md px-2 py-1.5 text-left outline-none focus-visible:ring-3 focus-visible:ring-ring/50"
        onClick={onOpen}
      >
        <span className="w-full truncate text-[0.8125rem] text-foreground">{title}</span>
        <span className="text-xs text-muted-foreground">
          {conversation.active ? 'Current chat · ' : ''}
          <time dateTime={updated}>{relativeTime(updated)}</time>
        </span>
      </button>
      {/* on a desktop the actions show on hover or keyboard focus; on touch screens always */}
      <Tip label="Rename">
        <Button variant="ghost" size="icon-xs" aria-label={`Rename “${title}”`} className={cn('text-muted-foreground', ROW_ACTION)} onClick={onRename}>
          <Edit />
        </Button>
      </Tip>
      <Tip label="Delete">
        <Button variant="ghost" size="icon-xs" aria-label={`Delete “${title}”`} className={cn('text-muted-foreground hover:text-destructive', ROW_ACTION)} onClick={onDelete}>
          <Trash />
        </Button>
      </Tip>
    </li>
  )
}

function RenameRow({ conversation, onDone, onCancel }: { conversation: ConversationSummary; onDone: (title: string) => void; onCancel: () => void }) {
  const [value, setValue] = useState(chatTitle(conversation) === UNTITLED ? '' : chatTitle(conversation))
  function onKeyDown(event: KeyboardEvent<HTMLInputElement>) {
    if (event.key === 'Escape') {
      // only the rename ends, not the panel
      event.preventDefault()
      event.stopPropagation()
      onCancel()
    }
  }
  return (
    <li className="px-1 py-1">
      <form
        className="flex items-center gap-1.5"
        onSubmit={(event: FormEvent) => {
          event.preventDefault()
          onDone(value)
        }}
      >
        <Input aria-label="Chat name" autoFocus maxLength={120} value={value} onChange={(event) => setValue(event.target.value)} onKeyDown={onKeyDown} onFocus={(event) => event.currentTarget.select()} />
        <Button type="submit" size="sm" disabled={!value.trim()}>
          Save
        </Button>
        <Button type="button" size="sm" variant="ghost" onClick={onCancel}>
          Cancel
        </Button>
      </form>
    </li>
  )
}
