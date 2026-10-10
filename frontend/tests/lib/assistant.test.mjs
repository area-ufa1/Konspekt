import test from 'node:test'
import assert from 'node:assert/strict'
import { applyAnswerEvent, mergeLiveTurns, clock } from '../../src/lib/assistant.ts'

const pending = { id: 'a', askedAtSec: 65, question: 'q', answer: '', model: '', createdAt: '', status: 'pending' }

test('streamed text replaces the answer, done finishes it', () => {
  let turns = applyAnswerEvent([pending], { id: 'a', text: 'При', done: false })
  assert.equal(turns[0].answer, 'При')
  assert.equal(turns[0].status, 'pending')
  turns = applyAnswerEvent(turns, { id: 'a', text: 'Привет', done: true })
  assert.equal(turns[0].answer, 'Привет')
  assert.equal(turns[0].status, 'done')
})

test('events for unknown turns change nothing', () => {
  const turns = [pending]
  assert.equal(applyAnswerEvent(turns, { id: 'zzz', text: 'x', done: true }), turns)
})

test('saved turns fill in after a reload without duplicates', () => {
  const saved = [{ id: 'a', askedAtSec: 65, question: 'q', answer: 'готово', model: 'haiku', createdAt: 't' },
                 { id: 'b', askedAtSec: 10, question: 'раньше', answer: 'ok', model: 'haiku', createdAt: 't' }]
  const merged = mergeLiveTurns([pending], saved)
  assert.deepEqual(merged.map((t) => t.id), ['b', 'a'])
  assert.equal(merged.find((t) => t.id === 'a').status, 'done')
})

test('clock matches the transcript', () => {
  assert.equal(clock(65), '1:05')
  assert.equal(clock(3723), '1:02:03')
})
