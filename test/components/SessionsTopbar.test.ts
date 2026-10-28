import { beforeEach, describe, expect, it } from 'vitest'
import { mount } from '@vue/test-utils'
import SessionsTopbar from '../../src/components/topbar/SessionsTopbar.vue'
import { vTooltip } from '../../src/tooltip'
import { setLang } from '../../src/settings'
import {
  resetSessionsToolbar,
  sessionSearch,
  sessionSort,
} from '../../src/sessionsToolbar'
import type { Agent, SessionMeta } from '../../src/types'

// Only the search bar + sort dropdown still live in the topbar; the with-id
// filter, select-mode entry, and batch operations were moved into
// SessionsView's body header (see `list-head-actions` in SessionsView.test.ts)
// to reduce the "two parallel icon rows" density at the top of the window.

beforeEach(() => {
  setLang('en')
  resetSessionsToolbar()
})

const session = (over: Partial<SessionMeta> = {}): SessionMeta => ({
  path: '/p/a.jsonl',
  fileName: 'a.jsonl',
  id: 'aaaa1111-bbbb-2222-cccc-333344445555',
  title: 'A session',
  cwd: '/p',
  size: 100,
  messageCount: 1,
  modified: 0,
  ...over,
})

const factory = (sessions: SessionMeta[] = [session(), session({ path: '/p/b.jsonl' })], agent?: Agent) =>
  mount(SessionsTopbar, {
    props: { sessions, agent },
    global: { directives: { tooltip: vTooltip } },
  })

describe('SessionsTopbar', () => {
  it('binds the search box to the shared search ref (debounced)', async () => {
    const wrapper = factory()
    await wrapper.find('.ct-search-input').setValue('parser')
    // 防抖：打字立即落到本地 draft，~220ms 后才同步到共享 ref
    expect(sessionSearch.value).toBe('')
    await new Promise((r) => setTimeout(r, 280))
    expect(sessionSearch.value).toBe('parser')
  })

  it('clears the search from the clear button', async () => {
    sessionSearch.value = 'parser'
    const wrapper = factory()
    await wrapper.find('.ct-search .ct-btn').trigger('click')
    expect(sessionSearch.value).toBe('')
  })

  it('lists every sort option and applies creation-time sorting', async () => {
    const wrapper = factory()
    await wrapper.find('.ct-scope-btn').trigger('click')
    const items = wrapper.findAll('.ct-scope-item')
    expect(items).toHaveLength(6)

    await items[2].trigger('click')
    expect(sessionSort.value).toBe('createdRecent')
  })

  it('adds reply-time sorting to Codex without removing the original six options', async () => {
    resetSessionsToolbar('codex')
    const wrapper = factory([session()], 'codex')
    expect(wrapper.find('.ct-scope-label').text()).toBe('Latest reply first')
    await wrapper.find('.ct-scope-btn').trigger('click')
    const items = wrapper.findAll('.ct-scope-item')
    expect(items.map((item) => item.text())).toEqual([
      'Newest first', 'Oldest first', 'Created: newest first', 'Created: oldest first',
      'Largest first', 'Most messages', 'Latest reply first',
    ])
    expect(items[6].attributes('aria-checked')).toBe('true')
    await items[0].trigger('click')
    expect(sessionSort.value).toBe('recent')
    expect(wrapper.find('.ct-scope-label').text()).toBe('Newest first')
    await wrapper.find('.ct-scope-btn').trigger('click')
    await wrapper.findAll('.ct-scope-item')[6].trigger('click')
    expect(sessionSort.value).toBe('replyRecent')
    expect(wrapper.find('.ct-scope-label').text()).toBe('Latest reply first')
    expect(wrapper.find('[role="menu"]').exists()).toBe(false)
    wrapper.unmount()
  })

  it('uses the requested Chinese label for the new sort option', async () => {
    setLang('zh')
    resetSessionsToolbar('codex')
    const wrapper = factory([session()], 'codex')
    expect(wrapper.find('.ct-scope-label').text()).toBe('回复优先')
    await wrapper.find('.ct-scope-btn').trigger('click')
    const reply = wrapper.findAll('.ct-scope-item').find((item) => item.text() === '回复优先')!
    expect(reply).toBeDefined()
    await reply.trigger('click')
    expect(wrapper.find('.ct-scope-label').text()).toBe('回复优先')
    wrapper.unmount()
  })

  it('keeps the original sort menu for sources without reply metadata', async () => {
    const wrapper = factory([session()], 'claude')
    await wrapper.find('.ct-scope-btn').trigger('click')
    expect(wrapper.findAll('.ct-scope-item')).toHaveLength(6)
    expect(wrapper.text()).not.toContain('Latest reply first')
    wrapper.unmount()
  })

  it('focuses the search box on the ⌘F / Ctrl+F shortcut', () => {
    const wrapper = mount(SessionsTopbar, {
      props: { sessions: [session(), session({ path: '/p/b.jsonl' })] },
      global: { directives: { tooltip: vTooltip } },
      attachTo: document.body,
    })
    const isMac = /Mac/i.test(navigator.platform)
    window.dispatchEvent(
      new KeyboardEvent('keydown', { key: 'f', metaKey: isMac, ctrlKey: !isMac }),
    )
    expect(document.activeElement).toBe(wrapper.find('.ct-search-input').element)
    wrapper.unmount()
  })

  it('does not render the action bar — those controls live in SessionsView now', () => {
    const wrapper = factory()
    expect(wrapper.find('.ct-actions').exists()).toBe(false)
  })
})
