import { useMemo, useRef, useState, type FormEvent, type ReactNode } from 'react'
import { Link, useNavigate } from 'react-router'
import { toast } from 'sonner'
import { useQueryClient } from '@tanstack/react-query'
import { DocumentDownload, DocumentUpload, Import, TickCircle, Warning } from 'reicon-react'
import type { OutlineImport, OutlineImportPreview, PageSummary, Teamspace } from '@orbit/api/generated/types.gen'
import { cn } from 'cn'
import { Button, buttonVariants } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { PasswordInput } from '@/components/ui/password-input'
import { Spinner } from '@/components/ui/spinner'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { EmptyState } from '@/components/common/EmptyState'
import { InfoTip } from '@/components/common/InfoTip'
import { Pane } from '@/components/common/Pane'
import { TitleTip } from '@/components/common/TitleTip'
import { confirmAction } from '@/components/common/confirmAction'
import { isActiveImport, isFinalImport, isImportInProgress } from '@/features/docs/api/notionImports'
import {
  OUTLINE_CLOUD_URL,
  connectOutline,
  fetchOutlineImportWarnings,
  outlineImportErrorMessage,
  reconcileOutlineImport,
  uploadOutlineExport,
  useCancelOutlineImport,
  useOutlineImport,
  useOutlineImportPreview,
  useOutlineImports,
  useStartOutlineImport,
  warningsCsv,
} from '@/features/docs/api/outlineImports'
import { isPageNotFound, usePageTree } from '@/features/docs/api/pages'
import { useTeamspaces } from '@/features/docs/api/teamspaces'
import { ImportBody, ImportHeader, ImportedPages, ProgressBar, RecentImports, Stat } from '@/features/docs/components/ImportParts'
import { ImportDestinationFields } from '@/features/docs/components/ImportDestinationFields'
import { destinationLabel, useImportDestination } from '@/features/docs/import/destination'
import { useCan } from '@/features/workspaces/permissions'

const TITLE = 'Import from Outline'
const NEW_IMPORT = '/docs/import/outline'

function plural(count: number, one: string, many = `${one}s`): string {
  return `${count} ${count === 1 ? one : many}`
}

type StepError = { message: string; inProgress: boolean }

/** Upload tab: drop or choose the export's .zip. */
function UploadExport({ workspaceId, onError }: { workspaceId: string; onError: (error: StepError | null) => void }) {
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const input = useRef<HTMLInputElement>(null)
  const [upload, setUpload] = useState<{ name: string; fraction: number; controller: AbortController } | null>(null)
  const [dropOver, setDropOver] = useState(false)

  const send = async (file: File | undefined) => {
    if (input.current) input.current.value = ''
    if (!file || upload) return
    if (!file.name.toLowerCase().endsWith('.zip')) {
      onError({ message: 'Upload the Outline export as one .zip file.', inProgress: false })
      return
    }
    const controller = new AbortController()
    onError(null)
    setUpload({ name: file.name, fraction: 0, controller })
    try {
      const created = await uploadOutlineExport(workspaceId, file, {
        signal: controller.signal,
        onProgress: (fraction) => setUpload((current) => (current ? { ...current, fraction } : current)),
      })
      reconcileOutlineImport(queryClient, workspaceId, created)
      navigate(`${NEW_IMPORT}/${created.id}`)
    } catch (caught) {
      if (caught instanceof DOMException && caught.name === 'AbortError') return
      onError({ message: outlineImportErrorMessage(caught), inProgress: isImportInProgress(caught) })
    } finally {
      setUpload(null)
    }
  }

  return (
    <div className="flex flex-col gap-4">
      <p className="text-sm text-muted-foreground">
        In Outline, open <span className="font-medium text-foreground">Settings → Export</span>, choose{' '}
        <span className="font-medium text-foreground">JSON</span> (recommended) or Markdown, and download the .zip Outline emails you.
      </p>
      {upload ? (
        <div className="flex flex-col gap-2" role="status" aria-live="polite">
          <div className="flex items-center gap-2 text-sm">
            <Spinner className="size-4 text-muted-foreground" />
            <span className="min-w-0 flex-1 truncate text-foreground">Uploading {upload.name}…</span>
            <span className="shrink-0 text-muted-foreground tabular-nums">{Math.round(upload.fraction * 100)}%</span>
          </div>
          <ProgressBar done={Math.round(upload.fraction * 100)} total={100} label="Upload progress" />
          <div>
            <Button type="button" variant="outline" size="sm" onClick={() => upload.controller.abort()}>
              Cancel upload
            </Button>
          </div>
        </div>
      ) : (
        <div
          data-slot="drop-zone"
          data-drop-over={dropOver || undefined}
          className="flex flex-col items-center gap-3 rounded-lg border border-dashed border-border px-4 py-8 text-center transition-[background-color,border-color] duration-150 ease-out data-drop-over:border-primary data-drop-over:bg-primary/5"
          onDragOver={(event) => {
            if (!event.dataTransfer.types.includes('Files')) return
            event.preventDefault()
            setDropOver(true)
          }}
          onDragLeave={(event) => {
            if (!event.currentTarget.contains(event.relatedTarget as Node | null)) setDropOver(false)
          }}
          onDrop={(event) => {
            if (!event.dataTransfer.types.includes('Files')) return
            event.preventDefault()
            setDropOver(false)
            void send(event.dataTransfer.files[0])
          }}
        >
          <DocumentUpload className="size-6 text-muted-foreground/70" />
          <p className="text-sm text-muted-foreground">Drop the export’s .zip file here, or</p>
          <input
            ref={input}
            type="file"
            accept=".zip,application/zip"
            className="hidden"
            aria-label="Outline export .zip file"
            onChange={(event) => void send(event.target.files?.[0])}
          />
          <Button type="button" variant="outline" onClick={() => input.current?.click()}>
            Choose file
          </Button>
        </div>
      )}
    </div>
  )
}

/** API tab: base URL and an API key. */
function ConnectApi({ workspaceId, onError }: { workspaceId: string; onError: (error: StepError | null) => void }) {
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const [baseUrl, setBaseUrl] = useState(OUTLINE_CLOUD_URL)
  const [token, setToken] = useState('')
  const [pending, setPending] = useState(false)

  const submit = async (event: FormEvent) => {
    event.preventDefault()
    const value = token.trim()
    if (!value || !baseUrl.trim() || pending) return
    setPending(true)
    onError(null)
    try {
      const created = await connectOutline(workspaceId, baseUrl.trim(), value)
      setToken('')
      reconcileOutlineImport(queryClient, workspaceId, created)
      navigate(`${NEW_IMPORT}/${created.id}`)
    } catch (caught) {
      onError({ message: outlineImportErrorMessage(caught), inProgress: isImportInProgress(caught) })
    } finally {
      setPending(false)
    }
  }

  return (
    <form className="flex flex-col gap-3" onSubmit={(event) => void submit(event)} autoComplete="off">
      <div className="grid gap-1.5">
        <Label htmlFor="outline-url" className="flex items-center gap-1.5">
          Outline URL <InfoTip text="https://app.getoutline.com for Outline’s cloud, or the address of your self-hosted Outline." />
        </Label>
        <Input
          id="outline-url"
          type="url"
          inputMode="url"
          spellCheck={false}
          autoCapitalize="off"
          value={baseUrl}
          onChange={(event) => setBaseUrl(event.target.value)}
        />
      </div>
      <div className="grid gap-1.5">
        <Label htmlFor="outline-token" className="flex items-center gap-1.5">
          API key{' '}
          <InfoTip text="Create one in Outline under Settings → API & Apps. Orbit keeps it encrypted only until the import finishes, and never shows or logs it." />
        </Label>
        <PasswordInput
          id="outline-token"
          name="outline-token"
          autoComplete="off"
          spellCheck={false}
          autoCapitalize="off"
          placeholder="ol_api_…"
          value={token}
          onChange={(event) => setToken(event.target.value)}
        />
      </div>
      <div>
        <Button type="submit" disabled={!token.trim() || !baseUrl.trim() || pending}>
          {pending ? <Spinner className="size-4" /> : null}
          Scan workspace
        </Button>
      </div>
    </form>
  )
}

/** Step 1: upload an export or connect the API. Also lists the admin's recent imports. */
function ConnectStep({ workspaceId }: { workspaceId: string }) {
  const imports = useOutlineImports(workspaceId)
  const [error, setError] = useState<StepError | null>(null)
  const running = imports.data?.find((item) => isActiveImport(item.status))
  const report = (next: StepError | null) => {
    setError(next)
    if (next?.inProgress) void imports.refetch()
  }
  return (
    <>
      <ImportHeader title={TITLE} />
      <ImportBody>
        <section className="flex flex-col gap-4 rounded-lg border border-border p-5">
          <TitleTip
            as="h2"
            title="Bring your Outline workspace"
            text="Each collection becomes a page with its documents nested inside, in Outline’s order, with their icons, dates and authors (members with the same email). Images and attachments are uploaded to Orbit, and links between documents point to the new pages. Importing again updates changed pages instead of duplicating them. Drafts are skipped."
            className="text-sm font-semibold text-foreground"
          />
          <Tabs defaultValue="upload" onValueChange={() => setError(null)}>
            <TabsList>
              <TabsTrigger value="upload">Upload export</TabsTrigger>
              <TabsTrigger value="api">Connect via API</TabsTrigger>
            </TabsList>
            <TabsContent value="upload" className="pt-3">
              <UploadExport workspaceId={workspaceId} onError={report} />
            </TabsContent>
            <TabsContent value="api" className="pt-3">
              <ConnectApi workspaceId={workspaceId} onError={report} />
            </TabsContent>
          </Tabs>
          {error ? (
            <p role="alert" className="text-sm text-destructive">
              {error.message}{' '}
              {error.inProgress && running ? (
                <Link className="text-primary underline underline-offset-2" to={`${NEW_IMPORT}/${running.id}`}>
                  Open the running import
                </Link>
              ) : null}
            </p>
          ) : null}
        </section>
        <RecentImports
          imports={imports.data}
          isPending={imports.isPending}
          name={(item) => item.file_name || 'Outline'}
          href={(item) => `${NEW_IMPORT}/${item.id}`}
        />
      </ImportBody>
    </>
  )
}

function useCancel(workspaceId: string, item: OutlineImport) {
  const cancel = useCancelOutlineImport(workspaceId)
  const request = async (started: boolean) => {
    const confirmed = await confirmAction({
      title: started ? 'Cancel the import?' : 'Cancel this import?',
      description: started
        ? 'Pages already imported stay. Empty pages the import created but did not fill yet are moved to the trash.'
        : 'Orbit deletes the uploaded export or the API key. You can start a new import at any time.',
      confirmLabel: 'Cancel import',
      cancelLabel: 'Keep going',
      danger: true,
    })
    if (!confirmed) return
    cancel.mutate({ importId: item.id }, { onError: (error) => toast.error(outlineImportErrorMessage(error)) })
  }
  return { request, pending: cancel.isPending }
}

function sourceLabel(item: OutlineImport): string {
  return item.source === 'api' ? (item.base_url ?? item.file_name) : item.file_name
}

/** Step 2: the scan job reads the export or Outline's API. */
function ScanningStep({ workspaceId, item }: { workspaceId: string; item: OutlineImport }) {
  const cancel = useCancel(workspaceId, item)
  return (
    <>
      <ImportHeader title={TITLE} detail={sourceLabel(item)} />
      <ImportBody>
        <div className="flex flex-col items-center gap-3 py-16 text-center" role="status">
          <Spinner className="size-6 text-muted-foreground" />
          <p className="text-sm font-medium text-foreground">{item.source === 'api' ? 'Reading your Outline workspace…' : `Reading “${item.file_name}”…`}</p>
          <p className="max-w-sm text-sm text-muted-foreground">Listing collections and documents. Large workspaces take a few minutes.</p>
          <Button type="button" variant="outline" size="sm" disabled={cancel.pending} onClick={() => void cancel.request(false)}>
            Cancel
          </Button>
        </div>
      </ImportBody>
    </>
  )
}

/** Step 3: the preview: collections with counts, options and the destination. */
function PreviewChooser({
  workspaceId,
  item,
  preview,
  teamspaces,
  pages,
}: {
  workspaceId: string
  item: OutlineImport
  preview: OutlineImportPreview
  teamspaces: Teamspace[] | undefined
  pages: PageSummary[] | undefined
}) {
  const [selected, setSelected] = useState<ReadonlySet<string>>(() => new Set(preview.collections.map((collection) => collection.key)))
  const [includeArchived, setIncludeArchived] = useState(false)
  const [includeTemplates, setIncludeTemplates] = useState(false)
  const destination = useImportDestination(teamspaces, pages)
  const start = useStartOutlineImport(workspaceId)
  const chosen = preview.collections.filter((collection) => selected.has(collection.key))
  const pagesToImport = chosen.reduce(
    (sum, collection) =>
      sum + 1 + collection.documents + (includeArchived ? collection.archived : 0) + (includeTemplates && collection.templates > 0 ? collection.templates + 1 : 0),
    0,
  )
  const archived = preview.collections.reduce((sum, collection) => sum + collection.archived, 0)
  const templates = preview.collections.reduce((sum, collection) => sum + collection.templates, 0)
  const allSelected = selected.size === preview.collections.length

  const toggle = (key: string, checked: boolean) => {
    const next = new Set(selected)
    if (checked) next.add(key)
    else next.delete(key)
    setSelected(next)
  }

  const submit = () => {
    if (chosen.length === 0) return
    start.mutate({
      importId: item.id,
      selection: allSelected ? { all: true } : { collections: chosen.map((collection) => collection.key) },
      options: { include_archived: includeArchived, include_templates: includeTemplates },
      destination: destination.body,
    })
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div role="status" aria-label="Export contents" className="flex flex-wrap items-center gap-x-3 gap-y-1 border-b border-border px-4 py-2 text-xs text-muted-foreground">
        <span>{preview.format === 'api' ? 'Outline API' : preview.format === 'json' ? 'JSON export' : 'Markdown export'}</span>
        <span>{plural(preview.collections.length, 'collection')}</span>
        {preview.too_large_files > 0 ? <span>{plural(preview.too_large_files, 'file')} over the upload limit will be skipped</span> : null}
        <span className="flex-1" />
        <Button type="button" variant="ghost" size="sm" onClick={() => setSelected(allSelected ? new Set() : new Set(preview.collections.map((collection) => collection.key)))}>
          {allSelected ? 'Clear' : 'Select all'}
        </Button>
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto">
        {preview.collections.length === 0 ? (
          <p className="p-6 text-center text-sm text-muted-foreground">The export has no collections to import.</p>
        ) : (
          <ul aria-label="Outline collections" className="flex flex-col">
            {preview.collections.map((collection) => (
              <li key={collection.key} className="border-b border-border">
                <Label className="flex min-h-11 cursor-pointer items-center gap-3 px-4 py-2 text-[0.8125rem] font-normal">
                  <Checkbox checked={selected.has(collection.key)} onCheckedChange={(checked) => toggle(collection.key, checked === true)} />
                  <span className="w-5 shrink-0 text-center">{collection.icon ?? ''}</span>
                  <span className="min-w-0 flex-1 truncate font-medium text-foreground">{collection.title || 'Untitled'}</span>
                  <span className="shrink-0 text-xs text-muted-foreground tabular-nums">
                    {[
                      plural(collection.documents, 'document'),
                      collection.archived > 0 ? `${collection.archived} archived or deleted` : null,
                      collection.templates > 0 ? plural(collection.templates, 'template') : null,
                      collection.attachments > 0 ? plural(collection.attachments, 'attachment') : null,
                    ]
                      .filter(Boolean)
                      .join(' · ')}
                  </span>
                </Label>
              </li>
            ))}
          </ul>
        )}
      </div>
      <div className="flex flex-col gap-3 border-t border-border px-3 py-3">
        <div className="flex flex-wrap gap-x-6 gap-y-2">
          <Label className="flex items-center gap-2 text-[0.8125rem] font-normal">
            <Checkbox checked={includeArchived} disabled={archived === 0} onCheckedChange={(checked) => setIncludeArchived(checked === true)} />
            Include archived and deleted documents
            <InfoTip text="Outline’s exports hold only published documents; archived documents and the trash are available when you connect the API." />
          </Label>
          <Label className="flex items-center gap-2 text-[0.8125rem] font-normal">
            <Checkbox checked={includeTemplates} disabled={templates === 0} onCheckedChange={(checked) => setIncludeTemplates(checked === true)} />
            Include templates
            <InfoTip text="Templates go to a “Templates” page in their collection. Outline’s exports have no templates; connect the API to import them." />
          </Label>
        </div>
        <div className="flex flex-wrap items-end gap-3">
          <ImportDestinationFields destination={destination} />
          <span className="flex-1" />
          <div className="flex flex-col items-end gap-1.5">
            {start.isError ? (
              <p role="alert" className="max-w-96 text-right text-xs text-destructive">
                {outlineImportErrorMessage(start.error)}
              </p>
            ) : null}
            <Button type="button" className="tabular-nums" disabled={chosen.length === 0 || start.isPending} onClick={submit}>
              Import {plural(pagesToImport, 'page')}
            </Button>
          </div>
        </div>
      </div>
    </div>
  )
}

function ChooseStep({ workspaceId, item }: { workspaceId: string; item: OutlineImport }) {
  const cancel = useCancel(workspaceId, item)
  const preview = useOutlineImportPreview(workspaceId, item.id)
  const tree = usePageTree(workspaceId)
  const teamspaces = useTeamspaces(workspaceId)
  return (
    <>
      <ImportHeader
        title="Choose collections"
        detail={sourceLabel(item)}
        actions={
          <Button type="button" variant="ghost" size="sm" disabled={cancel.pending} onClick={() => void cancel.request(false)}>
            Cancel
          </Button>
        }
      />
      {preview.data ? (
        <PreviewChooser workspaceId={workspaceId} item={item} preview={preview.data} teamspaces={teamspaces.data} pages={tree.data} />
      ) : preview.isError || (preview.isSuccess && !preview.data) ? (
        <div className="flex flex-1 flex-col items-center justify-center gap-3 p-6 text-center" role="alert">
          <p className="text-sm text-muted-foreground">{preview.isError ? outlineImportErrorMessage(preview.error) : 'The scan is not available.'}</p>
          <Button type="button" variant="outline" size="sm" onClick={() => void preview.refetch()}>
            Retry
          </Button>
        </div>
      ) : (
        <div className="flex flex-1 items-center justify-center">
          <Spinner className="text-muted-foreground" />
        </div>
      )}
    </>
  )
}

/** Step 4: the import job runs: documents and attachments done out of the total. */
function ImportingStep({ workspaceId, item }: { workspaceId: string; item: OutlineImport }) {
  const cancel = useCancel(workspaceId, item)
  const { done, total, failed, skipped, attachments_done: filesDone, attachments_total: filesTotal } = item.progress
  return (
    <>
      <ImportHeader title={TITLE} detail={sourceLabel(item)} />
      <ImportBody>
        <section className="flex flex-col gap-4 py-10" role="status" aria-live="polite">
          <div className="flex items-center gap-2">
            <Spinner className="size-4 text-muted-foreground" />
            <p className="text-sm font-medium text-foreground">{item.status === 'queued' ? 'Waiting for the import to start…' : 'Importing documents…'}</p>
          </div>
          <ProgressBar done={done + failed + skipped} total={total} label="Document progress" />
          <div className="flex flex-wrap items-center gap-x-4 gap-y-1 text-sm text-muted-foreground">
            <span>
              {done + skipped} of {total} documents
            </span>
            {filesTotal > 0 ? (
              <span>
                {filesDone} of {filesTotal} attachments
              </span>
            ) : null}
            {failed > 0 ? <span className="text-destructive">{failed} failed</span> : null}
            <span className="flex-1" />
            <Button type="button" variant="outline" size="sm" disabled={cancel.pending} onClick={() => void cancel.request(true)}>
              Cancel import
            </Button>
          </div>
          {filesTotal > 0 ? <ProgressBar done={filesDone} total={filesTotal} label="Attachment progress" /> : null}
          <p className="text-sm text-muted-foreground">You can leave this page; the import continues on the server and new pages appear in the sidebar.</p>
        </section>
      </ImportBody>
    </>
  )
}

function DownloadWarnings({ workspaceId, item }: { workspaceId: string; item: OutlineImport }) {
  const [pending, setPending] = useState(false)
  const download = async () => {
    setPending(true)
    try {
      const warnings = await fetchOutlineImportWarnings(workspaceId, item.id)
      const url = URL.createObjectURL(new Blob([warningsCsv(warnings)], { type: 'text/csv;charset=utf-8' }))
      const link = document.createElement('a')
      link.href = url
      link.download = `outline-import-warnings-${item.id}.csv`
      link.click()
      URL.revokeObjectURL(url)
    } catch (error) {
      toast.error(outlineImportErrorMessage(error))
    } finally {
      setPending(false)
    }
  }
  return (
    <Button type="button" variant="outline" size="sm" disabled={pending} onClick={() => void download()}>
      {pending ? <Spinner className="size-4" /> : <DocumentDownload className="size-4" />}
      Download warnings (CSV)
    </Button>
  )
}

/** Step 5: completed, failed, cancelled or expired, with the summary. */
function ResultStep({ workspaceId, item }: { workspaceId: string; item: OutlineImport }) {
  const tree = usePageTree(workspaceId)
  const teamspaces = useTeamspaces(workspaceId)
  const report = item.report
  const failed = item.status === 'failed'
  const destination = destinationLabel(item.destination, teamspaces.data, tree.data)
  const headline =
    item.status === 'completed' ? 'Import complete' : failed ? 'Import failed' : item.status === 'cancelled' ? 'Import cancelled' : 'Import expired'
  const notes = useMemo(() => {
    const out: string[] = []
    if (!report) return out
    if (report.files_failed > 0) out.push(`${plural(report.files_failed, 'attachment')} could not be imported.`)
    if (report.too_large_files > 0) out.push(`${plural(report.too_large_files, 'file')} over the upload limit ${report.too_large_files === 1 ? 'was' : 'were'} skipped.`)
    if (report.truncated_blocks > 0) out.push(`${plural(report.truncated_blocks, 'block')} ${report.truncated_blocks === 1 ? 'was' : 'were'} cut from documents that were too large.`)
    if (report.unfilled_pages_trashed > 0)
      out.push(`${plural(report.unfilled_pages_trashed, 'empty page')} the import had created ${report.unfilled_pages_trashed === 1 ? 'was' : 'were'} moved to the trash.`)
    return out
  }, [report])

  return (
    <>
      <ImportHeader title={TITLE} detail={sourceLabel(item)} />
      <ImportBody>
        <section className="flex flex-col gap-3">
          <div className="flex items-center gap-2">
            {item.status === 'completed' ? (
              <TickCircle className="size-5 text-primary" />
            ) : (
              <Warning className={cn('size-5', failed ? 'text-destructive' : 'text-muted-foreground')} />
            )}
            <h2 className="text-base font-semibold text-foreground">{headline}</h2>
          </div>
          {item.error ? (
            failed ? (
              <p role="alert" className="rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2 text-sm text-foreground">
                {item.error}
              </p>
            ) : (
              <p className="text-sm text-muted-foreground">{item.error}</p>
            )
          ) : null}
          {destination && item.progress.total > 0 ? <p className="text-sm text-muted-foreground">Destination: {destination}</p> : null}
        </section>
        {report ? (
          <div className="grid grid-cols-3 gap-2 max-[599px]:grid-cols-2">
            <Stat label="Pages created" value={report.pages_created} />
            <Stat label="Pages updated" value={report.pages_updated} />
            <Stat label="Unchanged, skipped" value={report.pages_unchanged} />
            <Stat label="Pages failed" value={item.progress.failed} />
            <Stat label="Attachments imported" value={report.files_imported} />
            <Stat label="Warnings" value={report.warnings} />
          </div>
        ) : null}
        <ImportedPages rootIds={item.root_page_ids} pages={tree.data} />
        {notes.length > 0 || (report && report.warnings > 0) ? (
          <section className="flex flex-col gap-2" aria-label="Conversion notes">
            <h3 className="text-xs font-medium text-muted-foreground">Conversion notes</h3>
            {notes.map((note) => (
              <p key={note} className="text-sm text-foreground">
                {note}
              </p>
            ))}
            {report && report.warnings > 0 ? (
              <div className="flex flex-wrap items-center gap-3">
                <p className="text-sm text-foreground">
                  {plural(report.warnings, 'warning')}: unsupported blocks, attachments that failed, and links or mentions that point outside the import.
                </p>
                <DownloadWarnings workspaceId={workspaceId} item={item} />
              </div>
            ) : null}
          </section>
        ) : null}
        {report && report.failures.length > 0 ? (
          <section className="flex flex-col gap-2" aria-label="Failed pages">
            <h3 className="text-xs font-medium text-muted-foreground">Failed pages</h3>
            <ul className="flex flex-col border-t border-border">
              {report.failures.map((failure) => (
                <li key={failure.key} className="flex flex-col gap-0.5 border-b border-border px-1 py-2">
                  <span className="text-[0.8125rem] text-foreground">{failure.title.trim() ? failure.title : 'Untitled'}</span>
                  <span className="text-xs text-muted-foreground">{failure.error}</span>
                </li>
              ))}
            </ul>
          </section>
        ) : null}
        <div>
          <Link className={buttonVariants({ variant: failed ? 'default' : 'outline' })} to={NEW_IMPORT}>
            <Import className="size-4" />
            {failed ? 'Try again' : 'Import more'}
          </Link>
        </div>
      </ImportBody>
    </>
  )
}

/**
 * `/docs/import/outline` (upload or connect + recent imports) and `/docs/import/outline/:importId` (one import through
 * its states), a pane next to the page tree like the other Docs imports. Workspace admins only.
 */
export function OutlineImportPane({ workspaceId, importId }: { workspaceId: string; importId?: string }) {
  const canImport = useCan('members.manage')
  const query = useOutlineImport(workspaceId, canImport ? importId : undefined)
  let content: ReactNode
  if (!canImport) {
    content = (
      <>
        <ImportHeader title={TITLE} />
        <div className="flex flex-1 flex-col p-6">
          <EmptyState icon={Import} title="Admins only" description="Only workspace admins can import from Outline, because an import keeps other members as page authors." />
        </div>
      </>
    )
  } else if (!importId) {
    content = <ConnectStep workspaceId={workspaceId} />
  } else if (query.data) {
    const item = query.data
    if (item.status === 'scanning') content = <ScanningStep workspaceId={workspaceId} item={item} />
    else if (item.status === 'ready') content = <ChooseStep key={item.id} workspaceId={workspaceId} item={item} />
    else if (item.status === 'queued' || item.status === 'importing') content = <ImportingStep workspaceId={workspaceId} item={item} />
    else if (isFinalImport(item.status)) content = <ResultStep workspaceId={workspaceId} item={item} />
  } else if (query.isError) {
    const missing = isPageNotFound(query.error)
    content = (
      <>
        <ImportHeader title={TITLE} />
        <div className="flex flex-1 flex-col p-6">
          <EmptyState
            icon={Import}
            title={missing ? 'Import not found' : 'Import unavailable'}
            description={missing ? 'This import does not exist or belongs to someone else.' : 'The server could not load this import.'}
            action={
              <div className="flex gap-2">
                {missing ? null : (
                  <Button type="button" variant="outline" onClick={() => void query.refetch()}>
                    Retry
                  </Button>
                )}
                <Link className={buttonVariants({ variant: 'outline' })} to={NEW_IMPORT}>
                  New import
                </Link>
              </div>
            }
          />
        </div>
      </>
    )
  } else {
    content = (
      <>
        <ImportHeader title={TITLE} />
        <div className="flex flex-1 items-center justify-center">
          <Spinner className="text-muted-foreground" />
        </div>
      </>
    )
  }
  return <Pane>{content}</Pane>
}
