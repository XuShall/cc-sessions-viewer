import type { SessionPage } from './types'

/** Complete the first page before displaying a sort that needs the whole project.
 * Check the current scope after each request so switching projects cannot apply
 * an old response or start an unnecessary full-project read.
 */
export async function loadSessionPage(
  fetchPage: (limit: number) => Promise<SessionPage>,
  options: {
    initialLimit: number
    needsAll: () => boolean
    isCurrent: () => boolean
  },
): Promise<SessionPage | null> {
  let page = await fetchPage(options.initialLimit)
  if (!options.isCurrent()) return null
  if (options.needsAll() && page.sessions.length < page.total) {
    page = await fetchPage(page.total)
    if (!options.isCurrent()) return null
  }
  return page
}
