/** IME confirmation keys belong to the input method, not composer shortcuts.
 * WebKit can report isComposing=false for the confirming Enter (keyCode 229).
 */
export function isImeKey(event: KeyboardEvent, composing: boolean): boolean {
  return composing || event.isComposing || event.keyCode === 229
}

/** Measure a separate textarea instead of collapsing the focused input to auto.
 * Resizing the live editor twice per keystroke disturbs its scroll/caret in old
 * WebViews, especially once it has reached its maximum height.
 */
export function resizeTextarea(el: HTMLTextAreaElement, maxHeight: number): void {
  if (!el.parentElement) return
  const style = getComputedStyle(el)
  const px = (value: string) => parseFloat(value) || 0
  const borders = px(style.borderLeftWidth) + px(style.borderRightWidth)
  const padding = px(style.paddingLeft) + px(style.paddingRight)
  const width = el.clientWidth > 0
    ? el.clientWidth + (style.boxSizing === 'border-box' ? borders : -padding)
    : parseFloat(style.width)
  const measure = el.cloneNode(false) as HTMLTextAreaElement
  measure.removeAttribute('id')
  measure.removeAttribute('name')
  measure.setAttribute('aria-hidden', 'true')
  measure.tabIndex = -1
  measure.value = el.value
  Object.assign(measure.style, {
    position: 'absolute',
    left: '-10000px',
    top: '0',
    width: Number.isFinite(width) ? `${width}px` : style.width,
    height: '0px',
    minHeight: '0px',
    maxHeight: 'none',
    visibility: 'hidden',
    pointerEvents: 'none',
    overflow: 'hidden',
    flex: 'none',
  })
  measure.style.setProperty('field-sizing', 'fixed')
  el.parentElement.appendChild(measure)
  try {
    const adjustment = style.boxSizing === 'border-box'
      ? px(style.borderTopWidth) + px(style.borderBottomWidth)
      : -px(style.paddingTop) - px(style.paddingBottom)
    const height = `${Math.min(measure.scrollHeight + adjustment, maxHeight)}px`
    if (el.style.height !== height) {
      const scrollTop = el.scrollTop
      el.style.height = height
      el.scrollTop = scrollTop
    }
  } finally {
    measure.remove()
  }
}
