import { expect, test } from 'bun:test'
import type { OutlineImport } from '@orbit/api/generated/types.gen'

import { importNotes, outlineImportProgress, warningsCsv } from './import-api'

function item(overrides: Partial<OutlineImport>): OutlineImport {
  return {
    id: 'imp-1',
    workspace_id: 'w',
    source: 'archive',
    status: 'importing',
    format: 'json',
    file_name: 'export.zip',
    archive_bytes: 1,
    base_url: null,
    progress: { total: 10, done: 3, failed: 1, skipped: 2, attachments_total: 4, attachments_done: 1 },
    error: null,
    destination: null,
    options: null,
    preview: null,
    report: null,
    root_page_ids: [],
    created_at: '2026-11-13T00:00:00Z',
    updated_at: '2026-11-13T00:00:00Z',
    ...overrides,
  }
}

test('progress counts documents and attachments', () => {
  expect(outlineImportProgress(item({}))).toEqual({ label: 'Importing documents…', step: null, done: 6, total: 10, detail: '5 of 10 documents · 1 of 4 attachments' })
  expect(outlineImportProgress(item({ status: 'queued' })).label).toBe('Waiting for the import to start…')
})

test('notes and warnings CSV', () => {
  const report = { pages_created: 1, pages_updated: 0, pages_unchanged: 0, files_imported: 0, files_failed: 2, too_large_files: 0, truncated_blocks: 1, warnings: 3, unfilled_pages_trashed: 0, failures: [] }
  expect(importNotes('outline', item({ status: 'completed', report }))).toEqual(['2 attachments could not be imported.', '1 block was cut from documents that were too large.'])
  expect(warningsCsv([{ title: 'A', kind: 'unresolved_link', detail: '/doc/x' }])).toBe('Page,Kind,Detail\r\nA,unresolved_link,/doc/x\r\n')
})
