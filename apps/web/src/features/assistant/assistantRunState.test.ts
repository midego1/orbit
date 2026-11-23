import { expect, test } from 'bun:test'
import { callSubject, deriveRun, edgeMotion, IDLE_RUN, statusText, syncDocumentTitle, taskKeys, titleFor, TITLE_PREFIX } from './assistantRunState'

type Part = { type: string; text?: string; toolName?: string; args?: unknown; result?: unknown; approval?: { approved?: boolean; resolution?: string } }
const user = (text: string) => ({ role: 'user', content: [{ type: 'text', text }] as Part[] })
const assistant = (...content: Part[]) => ({ role: 'assistant', content })
const call = (toolName: string, args: unknown, extra: Partial<Part> = {}): Part => ({ type: 'tool-call', toolName, args, ...extra })

const TASKS = JSON.stringify([
  { id: '0b6c9a52-8f43-4c3e-9a0e-1d2f3a4b5c6d', identifier: 'GEN-3', title: 'Write the launch post' },
  { id: '7a1e2b3c-4d5e-4f60-8a9b-0c1d2e3f4a5b', identifier: 'lch-4', title: 'Pick a date' },
])

test('a run goes idle → thinking → reading → waiting → changing → idle', () => {
  const asked = [user('Move GEN-3 to In Progress')]
  expect(deriveRun({ running: false, messages: asked })).toEqual(IDLE_RUN)
  expect(deriveRun({ running: true, messages: asked })).toEqual({ state: 'thinking', text: 'Thinking…' })

  const reading = [...asked, assistant(call('list_projects', {}))]
  expect(deriveRun({ running: true, messages: reading })).toEqual({ state: 'thinking', text: 'Reading your projects…' })

  const read = [...asked, assistant(call('list_tasks', {}, { result: TASKS }))]
  expect(deriveRun({ running: true, messages: read }).state).toBe('thinking')

  // the run stops on the interrupt: the approval card waits
  const update = { task_id: '0b6c9a52-8f43-4c3e-9a0e-1d2f3a4b5c6d', status_id: 's2' }
  const waiting = [...asked, assistant(call('list_tasks', {}, { result: TASKS }), call('update_task', update, { approval: {} }))]
  expect(deriveRun({ running: false, messages: waiting })).toEqual({ state: 'waiting', text: 'Waiting for your approval' })

  const approved = [...asked, assistant(call('list_tasks', {}, { result: TASKS }), call('update_task', update, { approval: { approved: true } }))]
  expect(deriveRun({ running: true, messages: approved })).toEqual({ state: 'changing', text: 'Changing GEN-3…' })

  const done = [...asked, assistant(call('list_tasks', {}, { result: TASKS }), call('update_task', update, { approval: { approved: true }, result: '{"ok":true}' }))]
  expect(deriveRun({ running: true, messages: done }).state).toBe('thinking')
  expect(deriveRun({ running: false, messages: done })).toEqual(IDLE_RUN)
})

test('waiting wins over a running stream, and a declined or resolved card no longer waits', () => {
  const waiting = [assistant(call('create_task', { title: 'A' }, { approval: {} }))]
  expect(deriveRun({ running: true, messages: waiting }).state).toBe('waiting')
  expect(deriveRun({ running: false, messages: [assistant(call('create_task', { title: 'A' }, { approval: { approved: false }, result: '{"declined":true}' }))] }).state).toBe('idle')
  expect(deriveRun({ running: false, messages: [assistant(call('create_task', { title: 'A' }, { approval: { resolution: 'expired' } }))] }).state).toBe('idle')
})

test('a change allowed for this chat runs without a card and still shows as changing', () => {
  const run = deriveRun({ running: true, messages: [assistant(call('create_task', { title: 'Write the launch post' }))] })
  expect(run).toEqual({ state: 'changing', text: 'Creating “Write the launch post”…' })
})

test('only the last message counts for the call in flight', () => {
  const earlier = assistant(call('update_task', { task_id: 'x' }))
  expect(deriveRun({ running: true, messages: [earlier, user('and now?')] }).state).toBe('thinking')
})

test('an approved change still running in the message that asked for it shows as changing after the resume', () => {
  const update = { task_id: '0b6c9a52-8f43-4c3e-9a0e-1d2f3a4b5c6d', status_id: 's2' }
  const asked = assistant(call('list_tasks', {}, { result: TASKS }), call('update_task', update, { approval: { approved: true } }))
  // the resumed run starts a message of its own, still empty
  expect(deriveRun({ running: true, messages: [user('Move GEN-3'), asked, assistant()] })).toEqual({ state: 'changing', text: 'Changing GEN-3…' })
  // one from an earlier message the user has moved on from does not count
  expect(deriveRun({ running: true, messages: [user('Move GEN-3'), asked, user('and now?'), assistant()] }).state).toBe('thinking')
})

test('status text names the task by its key, from the call or from earlier results', () => {
  const keys = taskKeys([assistant(call('list_tasks', {}, { result: TASKS }))])
  expect(keys.get('7a1e2b3c-4d5e-4f60-8a9b-0c1d2e3f4a5b')).toBe('LCH-4')
  expect(callSubject({ task_id: 'gen-12' })).toBe('GEN-12')
  expect(callSubject({ task_id: '7a1e2b3c-4d5e-4f60-8a9b-0c1d2e3f4a5b' }, keys)).toBe('LCH-4')
  expect(callSubject({ task_id: 'unknown-uuid-like-thing' })).toBe('')
  expect(callSubject({ name: '  A   very long project name that goes on and on and on  ' })).toBe('“A very long project name that goes on an…”')
  expect(statusText('changing', { toolName: 'update_task', args: { task_id: 'nope' } })).toBe('Making a change…')
  expect(statusText('changing', { toolName: 'delete_task', args: { task_id: 'GEN-3' } })).toBe('Deleting GEN-3…')
  expect(statusText('thinking', { toolName: 'get_task', args: { task_id: 'GEN-3' } })).toBe('Reading GEN-3…')
  expect(statusText('thinking', { toolName: 'list_docs', args: {} })).toBe('Reading your docs…')
  expect(statusText('thinking', { toolName: 'some_new_tool', args: {} })).toBe('Looking things up…')
  expect(statusText('idle')).toBe('')
})

test('a task list result nested in an object is read too, and odd values are ignored', () => {
  const nested = JSON.stringify({ tasks: [{ id: 't1', identifier: 'WEB-9' }, { id: 't2', identifier: '<b>x</b>' }, { id: 3, identifier: 'GEN-1' }] })
  const keys = taskKeys([assistant(call('list_tasks', {}, { result: nested })), assistant(call('list_tasks', {}, { result: 'not json' }))])
  expect([...keys]).toEqual([['t1', 'WEB-9']])
})

test('the tab title gets the prefix while waiting and loses only the prefix afterwards', () => {
  const doc = { title: 'Orbit' }
  syncDocumentTitle('thinking', doc)
  expect(doc.title).toBe('Orbit')
  syncDocumentTitle('waiting', doc)
  expect(doc.title).toBe(`${TITLE_PREFIX}Orbit`)
  syncDocumentTitle('waiting', doc)
  expect(doc.title).toBe(`${TITLE_PREFIX}Orbit`)
  // a page changes its title meanwhile: that title stays
  doc.title = `${TITLE_PREFIX}Tasks · Orbit`
  syncDocumentTitle('changing', doc)
  expect(doc.title).toBe('Tasks · Orbit')
  expect(titleFor('Orbit', 'idle')).toBe('Orbit')
})

test('the edge pulses only while thinking, and never with reduced motion', () => {
  expect(edgeMotion('idle', false)).toBe('none')
  expect(edgeMotion('thinking', false)).toBe('pulse')
  expect(edgeMotion('thinking', true)).toBe('steady')
  expect(edgeMotion('changing', false)).toBe('steady')
  expect(edgeMotion('waiting', false)).toBe('steady')
  expect(edgeMotion('waiting', true)).toBe('steady')
})
