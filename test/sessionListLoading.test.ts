import { afterEach, describe, expect, it, vi } from 'vitest'
import { loadSessionPage } from '../src/sessionListLoading'
import { filterSessions, resetSessionsToolbar, sessionSort, sessionsFilterActive } from '../src/sessionsToolbar'
import type { SessionMeta, SessionPage } from '../src/types'

function sessions(count: number): SessionMeta[] {
  return Array.from({ length: count }, (_, i) => ({
    id: String(i), fileName: `${i}.jsonl`, path: `/project/${i}.jsonl`, title: String(i),
    modified: count - i, lastReplyAt: i, size: 1, messageCount: 1,
    codexAppListScanned: 0, codexAppFirstPageSize: 50, codexAppFirstPagePosition: 0,
    codexInternal: false, codexArchived: false,
  }))
}

function deferred<T>() {
  let resolve!: (value: T) => void
  const promise = new Promise<T>((complete) => { resolve = complete })
  return { promise, resolve }
}

afterEach(() => resetSessionsToolbar())

describe('project session loading', () => {
  it('includes replies beyond the first 40 records before applying the Codex default sort', async () => {
    resetSessionsToolbar('codex')
    const all = sessions(80)
    const fetchPage = vi.fn(async (limit: number) => ({ total: all.length, sessions: all.slice(0, limit) }))
    const page = await loadSessionPage(fetchPage, {
      initialLimit: 40, needsAll: () => sessionsFilterActive.value, isCurrent: () => true,
    })
    expect(fetchPage.mock.calls.map(([limit]) => limit)).toEqual([40, 80])
    expect(page!.sessions).toHaveLength(80)
    expect(filterSessions(page!.sessions)[0].id).toBe('79')
  })

  it('loads all records again for the next Codex project even when the sort has not changed', async () => {
    for (const total of [41, 60]) {
      resetSessionsToolbar('codex')
      const all = sessions(total)
      const fetchPage = vi.fn(async (limit: number) => ({ total, sessions: all.slice(0, limit) }))
      const page = await loadSessionPage(fetchPage, {
        initialLimit: 40, needsAll: () => sessionsFilterActive.value, isCurrent: () => true,
      })
      expect(fetchPage.mock.calls.map(([limit]) => limit)).toEqual([40, total])
      expect(page!.sessions).toHaveLength(total)
    }
  })

  it('retains ordinary pagination for other agents and avoids extra reads for small projects', async () => {
    for (const [agent, total] of [['claude', 80], ['codex', 10]] as const) {
      resetSessionsToolbar(agent)
      const fetchPage = vi.fn(async (limit: number) => ({ total, sessions: sessions(total).slice(0, limit) }))
      const page = await loadSessionPage(fetchPage, {
        initialLimit: 40, needsAll: () => sessionsFilterActive.value, isCurrent: () => true,
      })
      expect(fetchPage).toHaveBeenCalledTimes(1)
      expect(page!.sessions).toHaveLength(Math.min(40, total))
    }
  })

  it('does not start a full-project read if the user switches projects during the first request', async () => {
    let current = true
    const pending = deferred<SessionPage>()
    const fetchPage = vi.fn(() => pending.promise)
    const loading = loadSessionPage(fetchPage, {
      initialLimit: 40, needsAll: () => true, isCurrent: () => current,
    })
    current = false
    pending.resolve({ total: 80, sessions: sessions(40) })
    expect(await loading).toBeNull()
    expect(fetchPage).toHaveBeenCalledTimes(1)
  })

  it('discards a full-project response if the user has moved to another project', async () => {
    let current = true
    const pending = deferred<SessionPage>()
    const fetchPage = vi.fn().mockResolvedValueOnce({ total: 80, sessions: sessions(40) })
      .mockReturnValueOnce(pending.promise)
    const loading = loadSessionPage(fetchPage, {
      initialLimit: 40, needsAll: () => true, isCurrent: () => current,
    })
    await vi.waitFor(() => expect(fetchPage).toHaveBeenCalledTimes(2))
    current = false
    pending.resolve({ total: 80, sessions: sessions(80) })
    expect(await loading).toBeNull()
  })

  it('honors a sort selection made while the first page is still loading', async () => {
    resetSessionsToolbar('claude')
    const pending = deferred<SessionPage>()
    const fetchPage = vi.fn().mockReturnValueOnce(pending.promise)
      .mockResolvedValueOnce({ total: 80, sessions: sessions(80) })
    const loading = loadSessionPage(fetchPage, {
      initialLimit: 40, needsAll: () => sessionsFilterActive.value, isCurrent: () => true,
    })
    sessionSort.value = 'replyRecent'
    pending.resolve({ total: 80, sessions: sessions(40) })
    expect((await loading)!.sessions).toHaveLength(80)
  })

  it('includes newly added sessions on refresh even when the previous list was complete', async () => {
    const all = sessions(81)
    const fetchPage = vi.fn(async (limit: number) => ({ total: 81, sessions: all.slice(0, limit) }))
    const page = await loadSessionPage(fetchPage, {
      initialLimit: 80, needsAll: () => true, isCurrent: () => true,
    })
    expect(fetchPage.mock.calls.map(([limit]) => limit)).toEqual([80, 81])
    expect(filterSessions(page!.sessions)).toHaveLength(81)
  })

  it('propagates a failed full-project request without entering a retry loop', async () => {
    const fetchPage = vi.fn().mockResolvedValueOnce({ total: 80, sessions: sessions(40) })
      .mockRejectedValueOnce(new Error('Read failed'))
    await expect(loadSessionPage(fetchPage, {
      initialLimit: 40, needsAll: () => true, isCurrent: () => true,
    })).rejects.toThrow('Read failed')
    expect(fetchPage).toHaveBeenCalledTimes(2)
  })
})
