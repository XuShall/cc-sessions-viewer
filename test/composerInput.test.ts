import { afterEach, describe, expect, it, vi } from 'vitest'
import { isImeKey, resizeTextarea } from '../src/composerInput'

afterEach(() => {
  vi.restoreAllMocks()
  document.body.replaceChildren()
})

describe('composer IME guard', () => {
  it.each([
    [true, false, 13, true],
    [false, true, 13, true],
    [false, false, 229, true],
    [false, false, 13, false],
  ])('handles composition=%s, event composition=%s, keyCode=%s', (composing, isComposing, keyCode, expected) => {
    expect(isImeKey(new KeyboardEvent('keydown', { key: 'Enter', isComposing, keyCode }), composing)).toBe(expected)
  })
})

describe('composer textarea fallback sizing', () => {
  function input() {
    const el = document.createElement('textarea')
    el.style.cssText = 'width:300px;height:220px;box-sizing:border-box;border:1px solid;padding:2px'
    el.value = 'long reference code\n'.repeat(40)
    document.body.appendChild(el)
    return el
  }

  it('measures a separate, hidden editor without collapsing the focused textarea', () => {
    const el = input()
    el.focus()
    el.setSelectionRange(12, 18)
    el.scrollTop = 180
    const scrollHeight = vi.spyOn(HTMLElement.prototype, 'scrollHeight', 'get').mockImplementation(function () {
      expect(this).not.toBe(el)
      expect(el.style.height).toBe('220px')
      expect((this as HTMLElement).style.visibility).toBe('hidden')
      expect((this as HTMLTextAreaElement).value).toBe(el.value)
      return 900
    })
    resizeTextarea(el, 220)
    expect(scrollHeight).toHaveBeenCalledOnce()
    expect(el.style.height).toBe('220px')
    expect(el.scrollTop).toBe(180)
    expect([el.selectionStart, el.selectionEnd]).toEqual([12, 18])
    expect(document.activeElement).toBe(el)
    expect(document.querySelectorAll('textarea')).toHaveLength(1)
  })

  it('can shrink again after deleting a large draft, including border-box borders', () => {
    const el = input()
    el.value = 'short draft'
    vi.spyOn(HTMLElement.prototype, 'scrollHeight', 'get').mockReturnValue(25)
    resizeTextarea(el, 220)
    expect(el.style.height).toBe('27px')
  })

  it('does not add padding twice for content-box inputs', () => {
    const el = input()
    el.style.boxSizing = 'content-box'
    vi.spyOn(HTMLElement.prototype, 'scrollHeight', 'get').mockReturnValue(25)
    resizeTextarea(el, 120)
    expect(el.style.height).toBe('21px')
  })

  it('uses the available width despite scrollbar and fractional zoom rounding', () => {
    const el = input()
    el.style.width = '300.7px'
    Object.defineProperties(el, { offsetWidth: { value: 300 }, clientWidth: { value: 293 } })
    vi.spyOn(HTMLElement.prototype, 'scrollHeight', 'get').mockImplementation(function () {
      expect((this as HTMLElement).style.width).toBe('295px')
      return 900
    })
    resizeTextarea(el, 220)
    expect(el.style.height).toBe('220px')
  })

  it('always removes the measurement node if layout measurement fails', () => {
    const el = input()
    vi.spyOn(HTMLElement.prototype, 'scrollHeight', 'get').mockImplementation(() => { throw new Error('layout failure') })
    expect(() => resizeTextarea(el, 220)).toThrow('layout failure')
    expect(document.querySelectorAll('textarea')).toHaveLength(1)
    expect(el.style.height).toBe('220px')
  })
})
