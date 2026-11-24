import { readFileSync } from 'node:fs'
import { expect, test } from '@playwright/test'
import { testWorkspace } from '../src/test/workspace'

const EXPORT = readFileSync(new URL('../../server/tests/fixtures/outline/json-export.zip', import.meta.url))

function outlineImport(status: string, extra: Record<string, unknown> = {}) {
  return {
    id: 'imp-1',
    workspace_id: 'alpha',
    source: 'archive',
    status,
    format: status === 'scanning' ? null : 'json',
    file_name: 'json-export.zip',
    archive_bytes: EXPORT.length,
    base_url: null,
    progress: { total: 0, done: 0, failed: 0, skipped: 0, attachments_total: 0, attachments_done: 0 },
    error: null,
    destination: null,
    options: null,
    preview: null,
    report: null,
    root_page_ids: [],
    created_at: '2026-11-13T00:00:00Z',
    updated_at: '2026-11-13T00:00:00Z',
    ...extra,
  }
}

const PREVIEW = {
  format: 'json',
  too_large_files: 0,
  collections: [
    { key: 'c:eng', title: 'Engineering', icon: '🛠️', documents: 3, archived: 0, templates: 0, attachments: 2 },
    { key: 'c:prod', title: 'Product', icon: null, documents: 1, archived: 0, templates: 0, attachments: 0 },
  ],
}

test('an admin uploads an Outline export, picks collections and gets a summary with warnings', async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 900 })
  let state = 'none'
  let started: Record<string, unknown> | null = null
  let polls = 0
  await page.route('**/api/v1/**', async (route) => {
    const request = route.request()
    const url = new URL(request.url())
    const path = url.pathname
    if (/\/views\/[^/]+\/favorite$|\/view-favorites\/order$/.test(path)) return route.fulfill({ status: 204 })
    if (path === '/api/v1/setup/status') return route.fulfill({ json: { complete: true } })
    if (path === '/api/v1/auth/me') return route.fulfill({ json: { id: 'user-1', display_name: 'Test User', email: 'test@example.com' } })
    if (path === '/api/v1/workspaces') return route.fulfill({ json: [testWorkspace('owner', { id: 'alpha', name: 'Alpha' })] })
    if (path === '/api/v1/workspaces/alpha/imports/outline/upload') {
      expect(request.method()).toBe('POST')
      expect(request.headers()['content-type']).toContain('multipart/form-data')
      state = 'scanning'
      return route.fulfill({ status: 201, json: outlineImport('scanning') })
    }
    if (path === '/api/v1/workspaces/alpha/imports/outline') return route.fulfill({ json: { items: state === 'none' ? [] : [outlineImport(state)] } })
    if (path === '/api/v1/workspaces/alpha/imports/outline/imp-1/start') {
      started = request.postDataJSON() as Record<string, unknown>
      state = 'importing'
      return route.fulfill({ json: outlineImport('queued', { progress: { total: 5, done: 0, failed: 0, skipped: 0, attachments_total: 2, attachments_done: 0 } }) })
    }
    if (path === '/api/v1/workspaces/alpha/imports/outline/imp-1/warnings') {
      return route.fulfill({ json: { items: [{ title: 'Roadmap', kind: 'unresolved_link', detail: '/doc/missing-MissDoc0001' }] } })
    }
    if (path === '/api/v1/workspaces/alpha/imports/outline/imp-1') {
      if (url.searchParams.get('include') === 'preview') return route.fulfill({ json: outlineImport('ready', { preview: PREVIEW }) })
      if (state === 'scanning') {
        state = 'ready'
        return route.fulfill({ json: outlineImport('scanning') })
      }
      if (state === 'importing') {
        polls += 1
        if (polls > 1) state = 'completed'
        return route.fulfill({
          json: outlineImport('importing', { progress: { total: 5, done: 2, failed: 0, skipped: 0, attachments_total: 2, attachments_done: 1 } }),
        })
      }
      if (state === 'completed') {
        return route.fulfill({
          json: outlineImport('completed', {
            destination: { teamspace_id: null, private: true, parent_page_id: null },
            options: { include_archived: false, include_templates: false },
            progress: { total: 5, done: 5, failed: 0, skipped: 0, attachments_total: 2, attachments_done: 2 },
            report: {
              pages_created: 5,
              pages_updated: 0,
              pages_unchanged: 0,
              files_imported: 2,
              files_failed: 0,
              too_large_files: 0,
              truncated_blocks: 0,
              warnings: 1,
              unfilled_pages_trashed: 0,
              failures: [],
            },
          }),
        })
      }
      return route.fulfill({ json: outlineImport(state) })
    }
    if (path.endsWith('/teamspaces')) return route.fulfill({ json: { items: [] } })
    return route.fulfill({ json: { items: [], next_cursor: null } })
  })

  await page.goto('/settings/import?workspace=alpha')
  await page.getByRole('link', { name: 'Import from Outline' }).click()
  await expect(page).toHaveURL(/\/docs\/import\/outline/)
  await expect(page.getByRole('tab', { name: 'Upload export' })).toBeVisible()
  await expect(page.getByRole('tab', { name: 'Connect via API' })).toBeVisible()
  await page.getByLabel('Outline export .zip file').setInputFiles({ name: 'json-export.zip', mimeType: 'application/zip', buffer: EXPORT })

  // The preview: collections with their counts, all checked.
  await expect(page.getByRole('heading', { name: 'Choose collections' })).toBeVisible()
  const collections = page.getByRole('list', { name: 'Outline collections' })
  await expect(collections.getByText('Engineering')).toBeVisible()
  await expect(collections.getByText('3 documents · 2 attachments')).toBeVisible()
  await expect(collections.getByText('1 document', { exact: true })).toBeVisible()
  await collections.getByText('Product').click()
  await expect(page.getByRole('button', { name: 'Import 4 pages' })).toBeEnabled()
  await page.getByRole('button', { name: 'Import 4 pages' }).click()
  await expect.poll(() => started).not.toBeNull()
  expect(started).toMatchObject({ selection: { collections: ['c:eng'] }, options: { include_archived: false, include_templates: false } })

  // Progress, then the summary with the warning list.
  await expect(page.getByText('Import complete')).toBeVisible({ timeout: 20_000 })
  await expect(page.getByText('Pages created')).toBeVisible()
  const download = page.waitForEvent('download')
  await page.getByRole('button', { name: 'Download warnings (CSV)' }).click()
  const file = await download
  expect(file.suggestedFilename()).toBe('outline-import-warnings-imp-1.csv')
  const csv = readFileSync(await file.path(), 'utf8')
  expect(csv).toContain('Roadmap,unresolved_link,/doc/missing-MissDoc0001')
})

test('members who are not admins do not see the Outline import', async ({ page }) => {
  await page.route('**/api/v1/**', async (route) => {
    const path = new URL(route.request().url()).pathname
    if (path === '/api/v1/setup/status') return route.fulfill({ json: { complete: true } })
    if (path === '/api/v1/auth/me') return route.fulfill({ json: { id: 'user-1', display_name: 'Test User', email: 'test@example.com' } })
    if (path === '/api/v1/workspaces') return route.fulfill({ json: [testWorkspace('member', { id: 'alpha', name: 'Alpha' })] })
    return route.fulfill({ json: { items: [], next_cursor: null } })
  })
  await page.goto('/settings/import?workspace=alpha')
  await expect(page.getByRole('link', { name: 'Import from Obsidian' })).toBeVisible()
  await expect(page.getByRole('link', { name: 'Import from Outline' })).toHaveCount(0)
  await page.goto('/docs/import/outline?workspace=alpha')
  await expect(page.getByText('Admins only')).toBeVisible()
})
