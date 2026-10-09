import test from 'node:test'
import assert from 'node:assert/strict'
import { isClaudeCliReady, claudeCliBlockingReason, supportsHaiku55 } from '../../src/lib/claude-cli-status.ts'

const ready = { installed: true, path: 'C:\claude.exe', version: '2.1.293 (Claude Code)', auth: { loggedIn: true }, api_key_env_detected: false, error: null }

test('ready only when installed and logged in', () => {
  assert.equal(isClaudeCliReady(ready), true)
  assert.equal(isClaudeCliReady({ ...ready, auth: { loggedIn: false } }), false)
  assert.equal(isClaudeCliReady(null), false)
})

test('blocking reason names what to fix', () => {
  assert.equal(claudeCliBlockingReason(null), 'not-installed')
  assert.equal(claudeCliBlockingReason({ ...ready, installed: false }), 'not-installed')
  assert.equal(claudeCliBlockingReason({ ...ready, auth: null }), 'not-logged-in')
  assert.equal(claudeCliBlockingReason(ready), null)
})

test('Haiku 5.5 needs Claude Code 2.1.293 or newer', () => {
  assert.equal(supportsHaiku55('2.1.292 (Claude Code)'), false)
  assert.equal(supportsHaiku55('2.1.293 (Claude Code)'), true)
  assert.equal(supportsHaiku55('2.2.0'), true)
  assert.equal(supportsHaiku55('3.0.1'), true)
  assert.equal(supportsHaiku55(null), false)
  assert.equal(supportsHaiku55('garbage'), false)
})
