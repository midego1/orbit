// The rows of the Ask AI "@" picker: tasks, projects, docs and people, from the same searches as the rest of the app.
import { useMemo } from 'react'
import { useDebouncedValue } from '@/lib/useDebouncedValue'
import { usePageSearch } from '@/features/docs/api/pages'
import { useRecentPages } from '@/features/docs/api/pageOptions'
import { useProjects } from '@/features/tasks/api/projects'
import { looksLikeTaskId, useTaskReferences, type TaskReference } from '@/features/tasks/taskReferences'
import { useTeamMembers } from '@/features/workspaces/api'
import type { User as Member } from '@/features/workspaces/models'
import { mentionLabel, type Mention, type MentionKind } from './composer'

/** One row of the "@" picker. */
export interface MentionOption {
  mention: Mention
  /** The item's own name (a task's title), shown next to the label. */
  title: string
  detail?: string
  task?: TaskReference
  member?: Member
}

export const GROUP: Record<MentionKind, string> = { task: 'Tasks', project: 'Projects', doc: 'Docs', person: 'People' }
const PER_GROUP = 4
const MAX_OPTIONS = 12

const matches = (query: string, ...values: (string | undefined)[]) => {
  const needle = query.trim().toLowerCase()
  return !needle || values.some((value) => value?.toLowerCase().includes(needle))
}

/** The picker's rows for "@query": tasks, projects, docs and people, from the same searches as the rest of the app. */
export function useMentionOptions(workspaceId: string, query: string | null): { options: MentionOption[]; loading: boolean } {
  const open = query !== null
  const debounced = useDebouncedValue(query, 150)
  const search = (debounced ?? '').trim()
  const tasks = useTaskReferences(workspaceId, search, open && debounced !== null)
  const pages = usePageSearch(workspaceId, open ? search : '')
  const recent = useRecentPages(workspaceId, open && !search, PER_GROUP)
  const projects = useProjects(workspaceId, open)
  const members = useTeamMembers(open ? workspaceId : null)
  const options = useMemo(() => {
    if (query === null) return []
    const q = query.trim()
    const taskRows: MentionOption[] = (tasks.data ?? []).slice(0, PER_GROUP).map((task) => ({
      mention: { kind: 'task', id: task.id, label: mentionLabel('task', task.title, task.identifier) },
      title: task.title,
      detail: task.projectName,
      task,
    }))
    const projectRows: MentionOption[] = (projects.data ?? [])
      .filter((project) => matches(q, project.name, project.key))
      .slice(0, PER_GROUP)
      .map((project) => ({ mention: { kind: 'project', id: project.id, label: mentionLabel('project', project.name) }, title: project.name, detail: project.key }))
    const docRows: MentionOption[] = (search ? (pages.data ?? []) : (recent.data ?? []))
      .slice(0, PER_GROUP)
      .map((page) => ({ mention: { kind: 'doc', id: page.id, label: mentionLabel('doc', page.title || 'Untitled') }, title: page.title || 'Untitled', detail: page.database_title ?? undefined }))
    const people: MentionOption[] = (members.data ?? [])
      .filter((member) => matches(q, member.name, member.handle))
      .slice(0, PER_GROUP)
      .map((member) => ({ mention: { kind: 'person', id: member.id, label: mentionLabel('person', member.name) }, title: member.name, detail: member.handle, member }))
    const groups = looksLikeTaskId(q) ? [taskRows, projectRows, docRows, people] : [people, taskRows, projectRows, docRows]
    return groups.flat().slice(0, MAX_OPTIONS)
  }, [query, search, tasks.data, projects.data, pages.data, recent.data, members.data])
  const loading = open && (debounced !== query || tasks.isFetching || pages.isFetching)
  return { options, loading }
}
