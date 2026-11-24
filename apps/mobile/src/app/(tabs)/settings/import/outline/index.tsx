import type { OutlineImport, OutlineImportPreview } from '@orbit/api/generated/types.gen'
import * as DocumentPicker from 'expo-document-picker'
import { router, useLocalSearchParams } from 'expo-router'
import { useRef, useState } from 'react'
import { Pressable, Share, View } from 'react-native'

import { type FlowSource, ImportFlow, NoteLine, ResultStep } from '../../../../../components/import/Flow'
import { Checkbox, ChooserFooter, ConnectError, ConnectSection, DestinationFields, ImportBody, ImportedPages, ImportScreen, Paragraph, RecentImports, Strong } from '../../../../../components/import/Parts'
import { Segmented } from '../../../../../components/settings/Shell'
import {
  OUTLINE_CLOUD_URL,
  connectOutline,
  destinationLabel,
  importErrorMessage,
  importNotes,
  isImportInProgress,
  outlineImportProgress,
  outlineWarnings,
  plural,
  uploadOutline,
  warningsCsv,
} from '../../../../../data/import-api'
import { useConnectImport, useImportDestination, useStartImport } from '../../../../../hooks/imports'
import { useOrbit } from '../../../../../orbit-context'
import { usePages, useTeamspaces } from '../../../../../queries'
import { useTheme } from '../../../../../theme'
import { Button, EmptyState, Icon, Input, LabelTip, Spinner, Text } from '../../../../../ui'
import { uploadPart } from '../../../../../upload-part'

const HOME = '/settings/import/outline'

const SOURCE: FlowSource<'outline'> = {
  kind: 'outline',
  title: 'Import from Outline',
  chooseTitle: 'Choose collections',
  home: HOME,
  detail: (item) => (item.source === 'api' ? (item.base_url ?? item.file_name) : item.file_name),
  scanning: (item) => ({
    headline: item.source === 'api' ? 'Reading your Outline workspace…' : `Reading “${item.file_name}”…`,
    text: 'Listing collections and documents. Large workspaces take a few minutes.',
  }),
  cancel: {
    before: 'Orbit deletes the uploaded export or the API key. You can start a new import at any time.',
    started: 'Pages already imported stay. Empty pages the import created but did not fill yet are moved to the trash.',
  },
  importing: {
    headline: (item) => (item.status === 'queued' ? 'Waiting for the import to start…' : 'Importing documents…'),
    unit: 'pages',
    note: 'You can leave this page; the import continues on the server and new pages appear in the sidebar.',
    progress: outlineImportProgress,
  },
  treeMissing: 'The scan is not available.',
}

/** The web's Outline import (`/docs/import/outline/:importId?`): upload an export or connect the API, choose collections, follow it. */
export default function OutlineImportScreen() {
  const { importId } = useLocalSearchParams<{ importId?: string }>()
  const { workspace } = useOrbit()
  if (!workspace.permissions.includes('members.manage')) {
    return (
      <ImportScreen title={SOURCE.title}>
        <View className="flex-1 p-6">
          <EmptyState icon="import" title="Admins only" description="Only workspace admins can import from Outline, because an import keeps other members as page authors." />
        </View>
      </ImportScreen>
    )
  }
  return <ImportFlow source={SOURCE} importId={importId} connect={<ConnectStep />} chooser={(item, preview) => <Chooser item={item} preview={preview} />} result={(item) => <Result item={item} />} />
}

type Mode = 'upload' | 'api'

function ConnectStep() {
  const { colors } = useTheme()
  const { client, workspaceId } = useOrbit()
  const connect = useConnectImport('outline', (created) => router.replace(`${HOME}/${created.id}`))
  const [mode, setMode] = useState<Mode>('upload')
  const [baseUrl, setBaseUrl] = useState(OUTLINE_CLOUD_URL)
  const [token, setToken] = useState('')
  const [upload, setUpload] = useState<{ name: string; controller: AbortController } | null>(null)
  const busy = useRef(false)
  const running = connect.error && isImportInProgress(connect.error) && connect.running ? `${HOME}/${connect.running.id}` : null

  const submit = () => {
    if (!token.trim() || !baseUrl.trim()) return
    void connect.submit(async () => {
      const created = await connectOutline(client, workspaceId, baseUrl.trim(), token.trim())
      setToken('')
      return created
    })
  }

  const choose = async () => {
    if (busy.current) return
    const picked = await DocumentPicker.getDocumentAsync({ type: ['application/zip', 'application/x-zip-compressed'], copyToCacheDirectory: true })
    const asset = picked.canceled ? undefined : picked.assets[0]
    if (!asset) return
    busy.current = true
    const controller = new AbortController()
    setUpload({ name: asset.name, controller })
    try {
      await connect.submit(async () => uploadOutline(client, workspaceId, await uploadPart(asset.uri, asset.name), asset.name, controller.signal))
    } finally {
      if (controller.signal.aborted) connect.setError(null)
      busy.current = false
      setUpload(null)
    }
  }

  return (
    <ImportScreen title={SOURCE.title}>
      <ImportBody>
        <ConnectSection
          title="Bring your Outline workspace"
          gap="gap-2"
          help="Each collection becomes a page with its documents nested inside, in Outline’s order, with their icons, dates and authors (members with the same email). Images and attachments are uploaded to Orbit, and links between documents point to the new pages. Importing again updates changed pages instead of duplicating them. Drafts are skipped."
        >
          <View className="gap-3">
            <Segmented<Mode>
              label="Import source"
              value={mode}
              options={[
                { value: 'upload', label: 'Upload export' },
                { value: 'api', label: 'Connect via API' },
              ]}
              onChange={(next) => {
                connect.setError(null)
                setMode(next)
              }}
            />
            {mode === 'upload' ? (
              <View className="gap-3">
                <Paragraph>
                  In Outline, open <Strong>Settings → Export</Strong>, choose <Strong>JSON</Strong> (recommended) or Markdown, and download the .zip Outline emails you.
                </Paragraph>
                {upload ? (
                  <View accessibilityLiveRegion="polite" className="gap-2">
                    <View className="flex-row items-center gap-2">
                      <Spinner />
                      <Text numberOfLines={1} className="min-w-0 flex-1 text-[14px] leading-[20px]">
                        Uploading {upload.name}…
                      </Text>
                    </View>
                    <View className="flex-row">
                      <Button label="Cancel upload" variant="outline" size="sm" onPress={() => upload.controller.abort()} />
                    </View>
                  </View>
                ) : (
                  <View className="items-center gap-3 rounded-lg border border-dashed border-border px-4 py-8">
                    <Icon name="documentUpload" size={24} color={colors.mutedForeground} />
                    <Text className="text-center text-[14px] leading-[20px] text-muted-foreground">Choose the export’s .zip file</Text>
                    <Button label="Choose file" variant="outline" onPress={() => void choose()} />
                  </View>
                )}
              </View>
            ) : (
              <View className="gap-3">
                <Input
                  label="Outline URL"
                  help="https://app.getoutline.com for Outline’s cloud, or the address of your self-hosted Outline."
                  keyboardType="url"
                  autoComplete="off"
                  autoCapitalize="none"
                  autoCorrect={false}
                  value={baseUrl}
                  onChangeText={setBaseUrl}
                />
                <Input
                  label="API key"
                  help="Create one in Outline under Settings → API & Apps. Orbit keeps it encrypted only until the import finishes, and never shows or logs it."
                  secureTextEntry
                  autoComplete="off"
                  autoCapitalize="none"
                  autoCorrect={false}
                  placeholder="ol_api_…"
                  value={token}
                  onChangeText={setToken}
                  onSubmitEditing={submit}
                />
                <View className="flex-row">
                  <Button label="Scan workspace" loading={connect.pending && !upload} disabled={!token.trim() || !baseUrl.trim() || connect.pending} onPress={submit} />
                </View>
              </View>
            )}
            {connect.error ? <ConnectError message={importErrorMessage('outline', connect.error)} running={running} /> : null}
          </View>
        </ConnectSection>
        <RecentImports imports={connect.imports} name={(item) => item.file_name || 'Outline'} unit="pages" href={(item) => `${HOME}/${item.id}`} />
      </ImportBody>
    </ImportScreen>
  )
}

/** An option with its help behind a tap (the web's InfoTip). */
function OptionRow({ checked, disabled, label, help, onToggle }: { checked: boolean; disabled: boolean; label: string; help: string; onToggle: () => void }) {
  return (
    <Pressable disabled={disabled} onPress={onToggle} className={`min-h-8 flex-row items-center gap-2 ${disabled ? 'opacity-50' : ''}`}>
      <Checkbox label={label} checked={checked} disabled={disabled} onPress={onToggle} />
      <View className="min-w-0 flex-1">
        <LabelTip label={label} text={help} className="text-[14px] leading-[20px]" />
      </View>
    </Pressable>
  )
}

function Chooser({ item, preview }: { item: OutlineImport; preview: OutlineImportPreview }) {
  const pages = usePages()
  const teamspaces = useTeamspaces()
  const destination = useImportDestination(teamspaces.data, pages.data)
  const start = useStartImport('outline')
  const [selected, setSelected] = useState<ReadonlySet<string>>(() => new Set(preview.collections.map((collection) => collection.key)))
  const [includeArchived, setIncludeArchived] = useState(false)
  const [includeTemplates, setIncludeTemplates] = useState(false)
  const chosen = preview.collections.filter((collection) => selected.has(collection.key))
  const allSelected = selected.size === preview.collections.length
  const archived = preview.collections.reduce((sum, collection) => sum + collection.archived, 0)
  const templates = preview.collections.reduce((sum, collection) => sum + collection.templates, 0)
  const count = chosen.reduce(
    (sum, collection) => sum + 1 + collection.documents + (includeArchived ? collection.archived : 0) + (includeTemplates && collection.templates > 0 ? collection.templates + 1 : 0),
    0,
  )
  const toggle = (key: string) => {
    const next = new Set(selected)
    if (next.has(key)) next.delete(key)
    else next.add(key)
    setSelected(next)
  }
  const submit = () => {
    if (chosen.length === 0) return
    start.mutate({
      importId: item.id,
      body: {
        selection: allSelected ? { all: true } : { collections: chosen.map((collection) => collection.key) },
        options: { include_archived: includeArchived, include_templates: includeTemplates },
        destination: destination.body,
      },
    })
  }
  return (
    <View className="flex-1">
      <View accessibilityLabel="Export contents" className="flex-row flex-wrap items-center gap-x-3 gap-y-1 border-b border-border px-4 py-2">
        <Text className="text-[12px] leading-[16px] text-muted-foreground">
          {preview.format === 'api' ? 'Outline API' : preview.format === 'json' ? 'JSON export' : 'Markdown export'} · {plural(preview.collections.length, 'collection', 'collections')}
          {preview.too_large_files > 0 ? ` · ${plural(preview.too_large_files, 'file', 'files')} over the upload limit will be skipped` : ''}
        </Text>
        <View className="flex-1" />
        <Button label={allSelected ? 'Clear' : 'Select all'} variant="ghost" size="sm" onPress={() => setSelected(allSelected ? new Set() : new Set(preview.collections.map((collection) => collection.key)))} />
      </View>
      <View accessibilityLabel="Outline collections" className="flex-1">
        {preview.collections.length === 0 ? (
          <Text className="p-6 text-center text-[14px] leading-[20px] text-muted-foreground">The export has no collections to import.</Text>
        ) : (
          preview.collections.map((collection) => (
            <Pressable key={collection.key} onPress={() => toggle(collection.key)} className="min-h-11 flex-row items-center gap-3 border-b border-border px-4 py-2 active:bg-sidebar-accent/50">
              <Checkbox label={`Select ${collection.title}`} checked={selected.has(collection.key)} onPress={() => toggle(collection.key)} />
              {collection.icon ? <Text className="text-[14px]">{collection.icon}</Text> : null}
              <View className="min-w-0 flex-1">
                <Text numberOfLines={1} className="font-noto-medium text-[14px] leading-[20px]">
                  {collection.title || 'Untitled'}
                </Text>
                <Text numberOfLines={2} className="text-[12px] leading-[16px] text-muted-foreground">
                  {[
                    plural(collection.documents, 'document', 'documents'),
                    collection.archived > 0 ? `${collection.archived} archived or deleted` : null,
                    collection.templates > 0 ? plural(collection.templates, 'template', 'templates') : null,
                    collection.attachments > 0 ? plural(collection.attachments, 'attachment', 'attachments') : null,
                  ]
                    .filter(Boolean)
                    .join(' · ')}
                </Text>
              </View>
            </Pressable>
          ))
        )}
      </View>
      <View className="gap-2 border-t border-border px-4 py-3">
        <OptionRow
          label="Include archived and deleted documents"
          help="Outline’s exports hold only published documents; archived documents and the trash are available when you connect the API."
          checked={includeArchived}
          disabled={archived === 0}
          onToggle={() => setIncludeArchived(!includeArchived)}
        />
        <OptionRow
          label="Include templates"
          help="Templates go to a “Templates” page in their collection. Outline’s exports have no templates; connect the API to import them."
          checked={includeTemplates}
          disabled={templates === 0}
          onToggle={() => setIncludeTemplates(!includeTemplates)}
        />
      </View>
      <ChooserFooter error={start.isError ? importErrorMessage('outline', start.error) : null} label={`Import ${plural(count, 'page', 'pages')}`} disabled={chosen.length === 0 || start.isPending} onImport={submit}>
        <DestinationFields destination={destination} />
      </ChooserFooter>
    </View>
  )
}

function ShareWarnings({ item }: { item: OutlineImport }) {
  const { client, workspaceId } = useOrbit()
  const [pending, setPending] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const share = async () => {
    setPending(true)
    setError(null)
    try {
      const warnings = await outlineWarnings(client, workspaceId, item.id)
      await Share.share({ title: 'Outline import warnings', message: warningsCsv(warnings) })
    } catch (caught) {
      setError(importErrorMessage('outline', caught))
    } finally {
      setPending(false)
    }
  }
  return (
    <View className="gap-1">
      <View className="flex-row">
        <Button label="Share warnings (CSV)" icon="send" variant="outline" size="sm" loading={pending} onPress={() => void share()} />
      </View>
      {error ? <Text className="text-[12px] leading-[16px] text-destructive">{error}</Text> : null}
    </View>
  )
}

function Result({ item }: { item: OutlineImport }) {
  const pages = usePages()
  const teamspaces = useTeamspaces()
  const report = item.report
  const failed = item.status === 'failed'
  const destination = destinationLabel(item.destination, teamspaces.data, pages.data)
  const notes = [
    ...importNotes('outline', item).map((note) => <NoteLine key={note}>{note}</NoteLine>),
    ...(report && report.warnings > 0
      ? [
          <View key="warnings" className="gap-2">
            <NoteLine>{plural(report.warnings, 'warning', 'warnings')}: unsupported blocks, attachments that failed, and links or mentions that point outside the import.</NoteLine>
            <ShareWarnings item={item} />
          </View>,
        ]
      : []),
  ]
  return (
    <ResultStep
      source={SOURCE}
      item={item}
      summary={
        <>
          {item.error ? (
            failed ? (
              <Text accessibilityRole="alert" className="rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2 text-[14px] leading-[20px]">
                {item.error}
              </Text>
            ) : (
              <Text className="text-[14px] leading-[20px] text-muted-foreground">{item.error}</Text>
            )
          ) : null}
          {destination && item.progress.total > 0 ? <Text className="text-[14px] leading-[20px] text-muted-foreground">Destination: {destination}</Text> : null}
        </>
      }
      stats={
        report
          ? [
              ['Pages created', report.pages_created],
              ['Pages updated', report.pages_updated],
              ['Unchanged, skipped', report.pages_unchanged],
              ['Pages failed', item.progress.failed],
              ['Attachments imported', report.files_imported],
              ['Warnings', report.warnings],
            ]
          : null
      }
      columns={2}
      notes={notes}
      notesTitle="Conversion notes"
      failures={(report?.failures ?? []).map((failure) => ({ key: failure.key, title: failure.title.trim() ? failure.title : 'Untitled', error: failure.error }))}
      failuresTitle="Failed pages"
      again={failed ? { label: 'Try again', primary: true } : undefined}
    >
      <ImportedPages rootIds={item.root_page_ids} pages={pages.data} />
    </ResultStep>
  )
}
