import { useEffect, useRef } from 'react'
import { useMutation, useQuery, useQueryClient, type QueryClient } from '@tanstack/react-query'
import { apiClient } from '@/api/client'
import {
  cancelOutlineImport,
  connectOutlineImport,
  getOutlineImport,
  listOutlineImportWarnings,
  listOutlineImports,
  startOutlineImport,
} from '@orbit/api/generated/sdk.gen'
import type {
  OutlineImport,
  OutlineImportDestinationBody,
  OutlineImportOptions,
  OutlineImportPreview,
  OutlineImportSelectionBody,
  OutlineImportWarning,
} from '@orbit/api/generated/types.gen'
import { ApiProblem } from '@orbit/api/problem'
import { queryKeys } from '@/api/queryKeys'
import { isActiveImport, isFinalImport } from './notionImports'
import { UploadUnreachable, uploadImportArchive } from './obsidianImports'

/** How often an import that is still working is polled. */
export const OUTLINE_IMPORT_POLL_MS = 2000

/** Outline's cloud; a self-hosted Outline has its own address. */
export const OUTLINE_CLOUD_URL = 'https://app.getoutline.com'

/** A user-facing sentence for a failed Outline import request (upload, connect, start, cancel, load). */
export function outlineImportErrorMessage(error: unknown): string {
  if (error instanceof ApiProblem) {
    switch (error.code) {
      case 'forbidden':
        return 'Only workspace admins can import from Outline.'
      case 'import_in_progress':
        return 'You already have an import running. Wait for it to finish or cancel it.'
      case 'outline_archive_too_large':
        return 'The export is larger than the server’s upload limit.'
      case 'outline_archive_invalid':
        return 'Upload the Outline export as one .zip file.'
      case 'outline_url_invalid':
        return 'Enter the https:// address of your Outline workspace, for example https://app.getoutline.com.'
      case 'outline_token_invalid':
        return 'Outline did not accept the API key. Check that you copied all of it and that it has not expired.'
      case 'outline_unavailable':
        return 'Outline could not be reached at this address. Check the URL and try again.'
      case 'app_key_missing':
        return 'This server has no app key, so it cannot keep the API key safe. Ask the operator to set ORBIT_APP_KEY.'
      case 'outline_import_too_large':
        return 'An import can contain at most 10,000 pages. Select fewer collections.'
      case 'outline_import_state_conflict':
        return 'This import changed in the meantime (it may have expired or been cancelled).'
      case 'outline_import_not_found':
        return 'This import no longer exists.'
      case 'page_not_found':
        return 'The destination page no longer exists. Choose another one.'
      case 'teamspace_not_found':
        return 'The destination teamspace no longer exists. Choose another one.'
    }
    if (error.status === 413) return 'The export is larger than the server’s upload limit.'
    if (error.status === 422) return 'The request was not valid. Check your input and try again.'
  }
  if (error instanceof UploadUnreachable) return 'The server could not be reached. Check your connection and try again.'
  return 'Something went wrong. Try again.'
}

type UploadOptions = { onProgress?: (fraction: number) => void; signal?: AbortSignal }

/** Uploads an Outline export (.zip) as the `file` field and starts the scan. */
export function uploadOutlineExport(workspaceId: string, file: File, options: UploadOptions = {}): Promise<OutlineImport> {
  return uploadImportArchive<OutlineImport>(`/api/v1/workspaces/${workspaceId}/imports/outline/upload`, file, options)
}

/** Connects Outline's API with a base URL and an API key and starts the scan. */
export async function connectOutline(workspaceId: string, baseUrl: string, token: string): Promise<OutlineImport> {
  const { data } = await connectOutlineImport({
    client: apiClient,
    path: { workspace_id: workspaceId },
    body: { base_url: baseUrl, token },
    throwOnError: true,
  })
  if (!data) throw new Error('Connect response was empty.')
  return data
}

/** Stores a fresh import in the detail cache and refreshes the list. */
export function reconcileOutlineImport(queryClient: QueryClient, workspaceId: string, item: OutlineImport): void {
  queryClient.setQueryData(queryKeys.outlineImports.detail(workspaceId, item.id), item)
  void queryClient.invalidateQueries({ queryKey: queryKeys.outlineImports.list(workspaceId) })
}

export function useOutlineImports(workspaceId: string, enabled = true) {
  return useQuery({
    queryKey: queryKeys.outlineImports.list(workspaceId),
    enabled,
    queryFn: async () => {
      const { data } = await listOutlineImports({ client: apiClient, path: { workspace_id: workspaceId }, throwOnError: true })
      if (!data) throw new Error('Imports response was empty.')
      return data.items
    },
    refetchInterval: (query) => (query.state.data?.some((item) => isActiveImport(item.status)) ? OUTLINE_IMPORT_POLL_MS * 2 : false),
  })
}

/**
 * One import without its preview, polled only while a job works on it. When it reaches a final state the page
 * queries and the imports list are refreshed. The preview comes from `useOutlineImportPreview`.
 */
export function useOutlineImport(workspaceId: string, importId: string | undefined, pollMs = OUTLINE_IMPORT_POLL_MS) {
  const queryClient = useQueryClient()
  const query = useQuery({
    queryKey: queryKeys.outlineImports.detail(workspaceId, importId ?? ''),
    enabled: Boolean(importId),
    queryFn: async () => {
      const { data } = await getOutlineImport({
        client: apiClient,
        path: { workspace_id: workspaceId, import_id: importId! },
        throwOnError: true,
      })
      if (!data) throw new Error('Import response was empty.')
      return data
    },
    refetchInterval: (current) => (isActiveImport(current.state.data?.status) ? pollMs : false),
  })
  const status = query.data?.status
  const previous = useRef(status)
  useEffect(() => {
    const before = previous.current
    previous.current = status
    if (before !== undefined && before !== status && isFinalImport(status)) {
      void queryClient.invalidateQueries({ queryKey: queryKeys.pages.all(workspaceId) })
      void queryClient.invalidateQueries({ queryKey: queryKeys.outlineImports.list(workspaceId) })
    }
  }, [queryClient, status, workspaceId])
  return query
}

/** The preview of a `ready` import, fetched once with `?include=preview` and never polled. */
export function useOutlineImportPreview(workspaceId: string, importId: string) {
  return useQuery<OutlineImportPreview | null>({
    queryKey: queryKeys.outlineImports.preview(workspaceId, importId),
    staleTime: Infinity,
    refetchOnWindowFocus: false,
    queryFn: async () => {
      const { data } = await getOutlineImport({
        client: apiClient,
        path: { workspace_id: workspaceId, import_id: importId },
        query: { include: 'preview' },
        throwOnError: true,
      })
      if (!data) throw new Error('Import response was empty.')
      return data.preview
    },
  })
}

export async function fetchOutlineImportWarnings(workspaceId: string, importId: string): Promise<OutlineImportWarning[]> {
  const { data } = await listOutlineImportWarnings({
    client: apiClient,
    path: { workspace_id: workspaceId, import_id: importId },
    throwOnError: true,
  })
  return data?.items ?? []
}

/** A CSV cell: quoted when it holds a comma, quote or line break; a leading formula sign is neutralized. */
function csvCell(value: string): string {
  const safe = /^[=+\-@\t\r]/.test(value) ? `'${value}` : value
  return /[",\n\r]/.test(safe) ? `"${safe.replaceAll('"', '""')}"` : safe
}

/** The warnings as a CSV file (page, kind, detail). */
export function warningsCsv(warnings: readonly OutlineImportWarning[]): string {
  const rows = [['Page', 'Kind', 'Detail'], ...warnings.map((warning) => [warning.title, warning.kind, warning.detail])]
  return `${rows.map((row) => row.map(csvCell).join(',')).join('\r\n')}\r\n`
}

export function useStartOutlineImport(workspaceId: string) {
  const queryClient = useQueryClient()
  return useMutation<
    OutlineImport,
    Error,
    { importId: string; selection: OutlineImportSelectionBody; options: OutlineImportOptions; destination: OutlineImportDestinationBody }
  >({
    mutationFn: async ({ importId, selection, options, destination }) => {
      const { data } = await startOutlineImport({
        client: apiClient,
        path: { workspace_id: workspaceId, import_id: importId },
        body: { selection, options, destination },
        throwOnError: true,
      })
      if (!data) throw new Error('Start import response was empty.')
      return data
    },
    onSuccess: (item) => reconcileOutlineImport(queryClient, workspaceId, item),
    onError: (error, { importId }) => {
      if (error instanceof ApiProblem && error.status === 409) {
        void queryClient.invalidateQueries({ queryKey: queryKeys.outlineImports.detail(workspaceId, importId) })
      }
    },
  })
}

export function useCancelOutlineImport(workspaceId: string) {
  const queryClient = useQueryClient()
  return useMutation<OutlineImport, Error, { importId: string }>({
    mutationFn: async ({ importId }) => {
      const { data } = await cancelOutlineImport({
        client: apiClient,
        path: { workspace_id: workspaceId, import_id: importId },
        throwOnError: true,
      })
      if (!data) throw new Error('Cancel import response was empty.')
      return data
    },
    onSuccess: (item) => {
      reconcileOutlineImport(queryClient, workspaceId, item)
      void queryClient.invalidateQueries({ queryKey: queryKeys.pages.all(workspaceId) })
    },
    onError: (_error, { importId }) => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.outlineImports.detail(workspaceId, importId) })
    },
  })
}
