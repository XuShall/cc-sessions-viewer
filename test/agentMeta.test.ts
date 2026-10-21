import { describe, expect, it } from 'vitest'
import { AGENT_META, agentLabel, agentSupports, sessionCanManageFile, sessionHistoryOnly } from '../src/agentMeta'

describe('agent metadata', () => {
  it('distinguishes readable projected Codex history from natively resumable rollouts', () => {
    expect(sessionHistoryOnly('codex', 'codex://original-id')).toBe(true)
    expect(sessionHistoryOnly('codex', '/sessions/rollout-id.jsonl.zst')).toBe(false)
    expect(sessionHistoryOnly('codex', '/sessions/rollout-id.jsonl')).toBe(false)
    expect(sessionHistoryOnly('pi', 'codex://not-a-codex-record')).toBe(false)
    expect(sessionCanManageFile('codex', 'codex://original-id')).toBe(false)
    expect(sessionCanManageFile('codex', '/sessions/rollout-id.jsonl.zst')).toBe(false)
    expect(sessionCanManageFile('codex', '/sessions/rollout-id.jsonl')).toBe(true)
    expect(sessionCanManageFile('agy', '/logs/transcript_full.jsonl')).toBe(true)
  })

  it('defines Grok Build as history-capable but never GUI-chat-capable', () => {
    expect(agentLabel('grok')).toBe('Grok Build')
    expect(AGENT_META.grok.cliName).toBe('grok')
    expect(agentSupports('grok', 'history')).toBe(true)
    expect(agentSupports('grok', 'guiChat')).toBe(false)
    expect(agentSupports('grok', 'worktree')).toBe(true)
    expect(agentSupports('grok', 'stats')).toBe(true)
    expect(agentSupports('grok', 'pricing')).toBe(true)
  })

  it('defines Kimi Code as historical/terminal-capable without GUI chat', () => {
    expect(agentLabel('kimicode')).toBe('Kimi Code')
    expect(AGENT_META.kimicode.cliName).toBe('kimi')
    expect(agentSupports('kimicode', 'history')).toBe(true)
    expect(agentSupports('kimicode', 'terminal')).toBe(true)
    expect(agentSupports('kimicode', 'guiChat')).toBe(false)
    expect(agentSupports('kimicode', 'worktree')).toBe(true)
    expect(agentSupports('kimicode', 'stats')).toBe(true)
    expect(agentSupports('kimicode', 'pricing')).toBe(true)
  })
})
