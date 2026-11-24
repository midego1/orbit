import { expect, test } from 'bun:test'
import {
  commandPrompt,
  findMention,
  removeMention,
  insertMention,
  isOpenPrompt,
  matchingCommands,
  MAX_MENTIONS,
  mentionLabel,
  mentionQuery,
  mentionSegments,
  mentionsToSend,
  pageKind,
  parseSlash,
  readMentions,
  serializeMentions,
  slashQuery,
  suggestionsFor,
  SLASH_COMMANDS,
  type Mention,
} from './composer'

const labels = (path: string) => suggestionsFor(path).map((suggestion) => suggestion.label)

test('a task page suggests task prompts', () => {
  expect(labels('/tasks/ENG-12')).toEqual(['Summarize this task', 'What is blocking this?', 'Draft next steps as sub-issues'])
  // a task opened in a view is a task page too
  expect(pageKind('/views/0193abc/ENG-12')).toBe('task')
})

test('a task list or project suggests list prompts', () => {
  expect(labels('/tasks')).toEqual(['Summarize open tasks', 'What is overdue?', 'Plan this week', 'What is assigned to me?'])
  expect(labels('/views/0193abc')).toContain('What is overdue?')
  expect(labels('/tasks?project=0193')).toEqual(['Summarize this project', 'What is overdue?', 'Plan this week'])
  expect(pageKind('/tasks/projects/0193/settings/general')).toBe('project')
})

test('a doc page suggests doc prompts; the docs home and trash do not', () => {
  expect(labels('/docs/0193abc')).toEqual(['Summarize this doc', 'Turn this into tasks', 'What is still open here?'])
  expect(pageKind('/docs')).toBe('docs')
  expect(pageKind('/docs/trash')).toBe('docs')
  expect(pageKind('/docs/import/csv')).toBe('docs')
})

test('activity, chat and other pages have a sensible default', () => {
  expect(labels('/activity')[0]).toBe('What needs my attention?')
  expect(pageKind('/chat/0193')).toBe('chat')
  expect(labels('/profile/general')).toEqual(['What is assigned to me?', 'Summarize open tasks', 'Create a task', 'Open my docs'])
  for (const path of ['/tasks/ENG-1', '/tasks', '/docs/x', '/docs', '/chat', '/activity', '/', '/settings/ai']) {
    const count = suggestionsFor(path).length
    expect(count >= 3 && count <= 4).toBe(true)
  }
})

test('an open prompt fills the field; a closed one is sent', () => {
  expect(isOpenPrompt('Create a task: ')).toBe(true)
  expect(isOpenPrompt('What is assigned to me?')).toBe(false)
})

test('the slash menu opens for "/" and letters at the end of the draft only', () => {
  expect(slashQuery('/', 1)).toBe('')
  expect(slashQuery('/su', 3)).toBe('su')
  expect(slashQuery('/su', 1)).toBeNull()
  expect(slashQuery('/summarize now', 14)).toBeNull()
  expect(slashQuery('see /su', 7)).toBeNull()
  expect(matchingCommands('').map((command) => command.name)).toEqual(['summarize', 'plan', 'standup', 'new'])
  expect(matchingCommands('s').map((command) => command.name)).toEqual(['summarize', 'standup'])
  expect(matchingCommands('x')).toEqual([])
  // no /model: the workspace's model is an admin setting
  expect(SLASH_COMMANDS.some((command) => command.name === 'model')).toBe(false)
})

test('commands write a clear prompt for the page; /new is an action', () => {
  const summarize = SLASH_COMMANDS.find((command) => command.name === 'summarize')!
  expect(commandPrompt(summarize, '/tasks/ENG-1')).toBe('Summarize this task in a few short points.')
  expect(commandPrompt(summarize, '/docs/abc', 'in Dutch')).toBe('Summarize this doc in a few short points. in Dutch')
  expect(commandPrompt(SLASH_COMMANDS.find((command) => command.name === 'new')!, '/')).toBeNull()
  expect(parseSlash('/standup')?.command.name).toBe('standup')
  expect(parseSlash('/plan  for the launch')?.rest).toBe('for the launch')
  expect(parseSlash('/unknown thing')).toBeNull()
  expect(parseSlash('not /plan')).toBeNull()
})

test('"@" starts a mention at the start or after a space only', () => {
  expect(mentionQuery('@', 1)).toEqual({ start: 0, query: '' })
  expect(mentionQuery('ask @ENG', 8)).toEqual({ start: 4, query: 'ENG' })
  expect(mentionQuery('mail me@example.com', 19)).toBeNull()
  expect(mentionQuery('@ x', 3)).toBeNull()
  expect(mentionQuery('@a\nb', 4)).toBeNull()
  // a picked mention does not open the menu again
  expect(mentionQuery('see @ENG-12 ', 12, ['ENG-12'])).toBeNull()
  expect(mentionQuery('see @ENG-12', 11, ['ENG-12'])).toBeNull()
  expect(mentionQuery('see @ENG-1', 10, ['ENG-12'])).toEqual({ start: 4, query: 'ENG-1' })
})

test('a picked mention replaces the typed query with its label', () => {
  expect(insertMention('ask @en please', 4, 7, 'ENG-12')).toEqual({ text: 'ask @ENG-12  please', cursor: 12 })
  expect(mentionLabel('task', 'Fix login', 'ENG-12')).toBe('ENG-12')
  expect(mentionLabel('doc', '  Roadmap\n2027 ')).toBe('Roadmap 2027')
  expect(mentionLabel('doc', '   ')).toBe('Untitled')
})

test('only mentions still named in the text are sent, once each, with ids and no titles', () => {
  const mentions: Mention[] = [
    { kind: 'task', id: 't1', label: 'ENG-12' },
    { kind: 'doc', id: 'd1', label: 'Roadmap' },
    { kind: 'task', id: 't1', label: 'ENG-12' },
  ]
  const sent = mentionsToSend('Compare @ENG-12 with the plan', mentions)
  expect(sent).toEqual([{ kind: 'task', id: 't1', label: 'ENG-12' }])
  expect(JSON.parse(serializeMentions(sent))).toEqual([{ kind: 'task', id: 't1', token: 'ENG-12' }])
  const many = Array.from({ length: 15 }, (_, index): Mention => ({ kind: 'person', id: `u${index}`, label: `P${index}` }))
  expect(mentionsToSend(many.map((mention) => `@${mention.label}`).join(' '), many)).toHaveLength(MAX_MENTIONS)
})

test('a sent message splits into text and mention chips, longest label first', () => {
  expect(mentionSegments('See @Roadmap 2027 and @Roadmap', [
    { kind: 'doc', token: 'Roadmap' },
    { kind: 'doc', token: 'Roadmap 2027' },
  ])).toEqual([
    { text: 'See ' },
    { text: '@Roadmap 2027', mention: 'doc' },
    { text: ' and ' },
    { text: '@Roadmap', mention: 'doc' },
  ])
  expect(mentionSegments('no mentions', [])).toEqual([{ text: 'no mentions' }])
})

test('stored mentions are read defensively', () => {
  expect(readMentions([{ kind: 'task', token: 'ENG-1' }, { kind: 'script', token: 'x' }, { kind: 'doc' }, null, 'x'])).toEqual([{ kind: 'task', token: 'ENG-1' }])
  expect(readMentions(undefined)).toEqual([])
})

test('a mention is found and removed only as a whole label, never inside a longer one', () => {
  expect(findMention('see @ENG-12 now', 'ENG-1')).toBe(-1)
  expect(findMention('see @ENG-12 and @ENG-1.', 'ENG-1')).toBe(16)
  expect(findMention('mail x@ENG-1', 'ENG-1')).toBe(-1)
  expect(removeMention('@ENG-1 @ENG-12 go', 'ENG-1')).toEqual({ text: '@ENG-12 go', cursor: 0 })
  expect(removeMention('@ENG-12 and @ENG-1', 'ENG-1')).toEqual({ text: '@ENG-12 and ', cursor: 12 })
  // only one occurrence goes
  expect(removeMention('@Ada and @Ada', 'Ada').text).toBe('and @Ada')
  expect(removeMention('nothing here', 'Ada')).toEqual({ text: 'nothing here', cursor: 12 })
})

test('a shorter label inside a longer mention is neither sent nor shown as a chip', () => {
  const mentions: Mention[] = [
    { kind: 'task', id: 't1', label: 'ENG-1' },
    { kind: 'task', id: 't12', label: 'ENG-12' },
  ]
  expect(mentionsToSend('only @ENG-12 here', mentions).map((mention) => mention.id)).toEqual(['t12'])
  expect(mentionSegments('only @ENG-12 here', [{ kind: 'task', token: 'ENG-1' }])).toEqual([{ text: 'only @ENG-12 here' }])
})
