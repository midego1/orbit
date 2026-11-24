import { expect, test } from 'bun:test'
import { ApiProblem } from '@orbit/api/problem'
import { outlineImportErrorMessage, warningsCsv } from './outlineImports'

test('warnings become a CSV with quoting and neutralized formulas', () => {
  const csv = warningsCsv([
    { title: 'Roadmap', kind: 'unresolved_link', detail: '/doc/missing-MissDoc0001' },
    { title: 'Q3, "draft"', kind: 'unsupported_block', detail: 'fancy_widget' },
    { title: '=HYPERLINK("x")', kind: 'attachment_failed', detail: 'line\nbreak' },
  ])
  expect(csv.split('\r\n')).toEqual([
    'Page,Kind,Detail',
    'Roadmap,unresolved_link,/doc/missing-MissDoc0001',
    '"Q3, ""draft""",unsupported_block,fancy_widget',
    `"'=HYPERLINK(""x"")",attachment_failed,"line\nbreak"`,
    '',
  ])
})

test('problems become sentences', () => {
  const problem = (code: string, status: number) =>
    new ApiProblem({ type: 'about:blank', title: 'x', status, code, detail: 'x', instance: '/', request_id: 'r' })
  expect(outlineImportErrorMessage(problem('forbidden', 403))).toBe('Only workspace admins can import from Outline.')
  expect(outlineImportErrorMessage(problem('outline_token_invalid', 422))).toContain('API key')
  expect(outlineImportErrorMessage(problem('outline_url_invalid', 422))).toContain('https://')
  expect(outlineImportErrorMessage(new Error('boom'))).toBe('Something went wrong. Try again.')
})
