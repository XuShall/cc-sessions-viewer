import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { mount, flushPromises, DOMWrapper } from '@vue/test-utils'
import ChatSidePanel from '../../src/components/ChatSidePanel.vue'
import CodexSidePanel from '../../src/components/CodexSidePanel.vue'
import type { ChatSession } from '../../src/chatSessions'

const { sendPromptMock } = vi.hoisted(() => ({ sendPromptMock: vi.fn().mockResolvedValue(undefined) }))
vi.mock('../../src/chatSessions', async () => {
  const { ref } = await import('vue')
  return { now: ref(Date.now()), sendPrompt: sendPromptMock, interruptChat: vi.fn() }
})
vi.mock('../../src/sideChat', () => ({ closeSideChat: vi.fn(), isBtwMinimized: () => false, setBtwMinimized: vi.fn() }))
vi.mock('../../src/codexSideChat', () => ({ closeCodexSideChat: vi.fn(), isCodexSideMinimized: () => false, setCodexSideMinimized: vi.fn() }))
vi.mock('../../src/panes', () => ({ focusedPane: { value: null } }))

beforeEach(() => { sendPromptMock.mockClear() })
afterEach(() => document.body.replaceChildren())

for (const [name, component, agent] of [
  ['Claude btw', ChatSidePanel, 'claude'],
  ['Codex side', CodexSidePanel, 'codex'],
] as const) {
  describe(`${name} input`, () => {
    function composer() {
      const session = {
        uiId: 92001, chatId: 1, agent, cwd: '/work/fixture', sessionId: 'fixture',
        status: 'running', turnState: 'idle', msgs: [], live: null,
      } as unknown as ChatSession
      return mount(component, {
        props: { session },
        attachTo: document.body,
        global: { directives: { tooltip: {} } },
      })
    }

    it.each([
      { label: 'native composition', active: false, isComposing: true, keyCode: 13 },
      { label: 'tracked composition', active: true, isComposing: false, keyCode: 13 },
      { label: 'WebKit confirming Enter', active: false, isComposing: false, keyCode: 229 },
    ])('does not send on $label, but accepts the next normal Enter', async (state) => {
      const wrapper = composer()
      try {
        await flushPromises()
        const input = new DOMWrapper(document.body.querySelector('textarea')!)
        await input.setValue('ame')
        if (state.active) await input.trigger('compositionstart')
        await input.trigger('keydown', { key: 'Enter', isComposing: state.isComposing, keyCode: state.keyCode })
        expect(sendPromptMock).not.toHaveBeenCalled()
        expect((input.element as HTMLTextAreaElement).value).toBe('ame')
        await input.trigger('compositionend')
        await input.trigger('keydown', { key: 'Enter', isComposing: false, keyCode: 13 })
        await flushPromises()
        expect(sendPromptMock).toHaveBeenCalledOnce()
        expect(sendPromptMock.mock.calls[0][1]).toBe('ame')
        expect((input.element as HTMLTextAreaElement).value).toBe('')
      } finally { wrapper.unmount() }
    })

    it('leaves Shift+Enter available for a newline', async () => {
      const wrapper = composer()
      try {
        const input = new DOMWrapper(document.body.querySelector('textarea')!)
        await input.setValue('draft')
        const event = new KeyboardEvent('keydown', { key: 'Enter', shiftKey: true, bubbles: true, cancelable: true })
        input.element.dispatchEvent(event)
        expect(event.defaultPrevented).toBe(false)
        expect(sendPromptMock).not.toHaveBeenCalled()
      } finally { wrapper.unmount() }
    })
  })
}
