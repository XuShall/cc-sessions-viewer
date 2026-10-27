import { beforeEach, describe, expect, it, vi } from 'vitest'
import { flushPromises, mount } from '@vue/test-utils'

const { invokeMock } = vi.hoisted(() => ({ invokeMock: vi.fn() }))

vi.mock('@tauri-apps/api/core', () => ({ invoke: invokeMock }))

import PricingView from '../../src/views/PricingView.vue'
import { enabledAgents, setLang } from '../../src/settings'
import { vTooltip } from '../../src/tooltip'

describe('PricingView Grok family', () => {
  beforeEach(() => {
    setLang('en')
    invokeMock.mockReset()
    invokeMock.mockImplementation((command: string) => {
      if (command === 'pricing_status') {
        return Promise.resolve({ loaded: true, fetching: false, lastError: null, modelCount: 1 })
      }
      if (command === 'list_pricing') {
        return Promise.resolve([
          {
            name: 'grok-test-priced',
            family: 'grok',
            input: 2e-6,
            output: 10e-6,
            cacheWrite: 2.5e-6,
            cacheRead: 0.2e-6,
            context: 131_072,
          },
        ])
      }
      return Promise.resolve(undefined)
    })
  })

  it('shows free input/output as zero without inventing unavailable cache rates', async () => {
    invokeMock.mockImplementation((command: string) => {
      if (command === 'pricing_status') {
        return Promise.resolve({ loaded: true, fetching: false, lastError: null, modelCount: 2 })
      }
      if (command === 'list_pricing') {
        return Promise.resolve([
          {
            name: 'new-free-preview', family: 'opencode',
            input: 0, output: 0, cacheWrite: 0, cacheRead: 0, context: 1_000_000,
          },
          {
            name: 'gemini-next-flash', family: 'agy',
            input: 0.75e-6, output: 3.75e-6, cacheWrite: 0, cacheRead: 0.075e-6,
            context: 1_048_576,
          },
        ])
      }
      return Promise.resolve(undefined)
    })
    const wrapper = mount(PricingView, {
      global: { directives: { tooltip: vTooltip } },
    })
    try {
      await flushPromises()
      const rows = wrapper.findAll('.pricing-row:not(.pricing-row-head)')
      const free = rows.find((row) => row.text().includes('new-free-preview'))
      expect(free?.findAll('[role="cell"]').map((cell) => cell.text())).toEqual([
        'new-free-preview', '1M', '$0.00', '$0.00', '—', '—',
      ])
      const gemini = rows.find((row) => row.text().includes('gemini-next-flash'))
      expect(gemini?.findAll('[role="cell"]').map((cell) => cell.text())).toEqual([
        'gemini-next-flash', '1.05M', '$0.75', '$3.75', '$0.075', '—',
      ])
    } finally {
      wrapper.unmount()
    }
  })

  it('renders and searches provider-qualified official models without mixing regional rates', async () => {
    invokeMock.mockImplementation((command: string) => {
      if (command === 'pricing_status') return Promise.resolve({ loaded: true, fetching: false, lastError: null, modelCount: 2 })
      if (command === 'list_pricing') return Promise.resolve([
        { name: 'moonshotai/kimi-k3.1-pro', family: 'other', input: 2e-6, output: 10e-6, cacheWrite: 0, cacheRead: 0.2e-6, context: 1_048_576 },
        { name: 'minimax/MiniMax-M3.1-Flash', family: 'other', input: 0.15e-6, output: 0.6e-6, cacheWrite: 0, cacheRead: 0.015e-6, context: 1_000_000 },
      ])
      return Promise.resolve(undefined)
    })
    const wrapper = mount(PricingView, { global: { directives: { tooltip: vTooltip } } })
    try {
      await flushPromises()
      expect(wrapper.findAll('.pricing-family').some(section => section.text().includes('Other official providers'))).toBe(true)
      const row = wrapper.findAll('.pricing-row').find(row => row.text().includes('moonshotai/kimi-k3.1-pro'))
      expect(row?.text()).toContain('$2.00')
      const input = wrapper.find('input')
      await input.setValue('MiniMax-M3.1')
      await input.trigger('keydown', { key: 'Enter' })
      expect(wrapper.text()).toContain('minimax/MiniMax-M3.1-Flash')
      expect(wrapper.text()).not.toContain('moonshotai/kimi-k3.1-pro')
      expect(wrapper.text()).toContain('Base USD reference rates')
    } finally { wrapper.unmount() }
  })

  it('renders the xAI family, Grok Build icon anchor, and model pricing row when Grok is hidden', async () => {
    const originalAgents = { ...enabledAgents.value }
    enabledAgents.value = { ...enabledAgents.value, grok: false }
    const wrapper = mount(PricingView, {
      global: { directives: { tooltip: vTooltip } },
    })
    try {
      await flushPromises()

      const grokSection = wrapper.findAll('.pricing-family').find((section) =>
        section.text().includes('Grok Build / xAI'),
      )
      expect(grokSection).toBeDefined()
      expect(grokSection?.text()).toContain('grok-test-priced')
      expect(grokSection?.text()).toContain('$2.00')
      expect(wrapper.find('.pricing-anchor.agent-grok img[alt="Grok Build"]').exists()).toBe(true)
    } finally {
      wrapper.unmount()
      enabledAgents.value = originalAgents
    }
  })
})
