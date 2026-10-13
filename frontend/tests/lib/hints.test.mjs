import test from 'node:test'
import assert from 'node:assert/strict'
import { parseHintAnswer, hintFromTurn, applyHintEvent } from '../../src/lib/hints.ts'

const ev = (id, at, state = 'done', extra = {}) =>
  ({ id, kind: 'hint', askedAtSec: at, question: 'q', short: 's', details: '', state, ...extra })

test('hint answers are JSON, anything else is the short part', () => {
  assert.deepEqual(parseHintAnswer('{"short":"Так.","details":"Потому."}'), { short: 'Так.', details: 'Потому.' })
  assert.deepEqual(parseHintAnswer('просто текст'), { short: 'просто текст', details: '' })
  assert.deepEqual(parseHintAnswer('{"oops":1}'), { short: '{"oops":1}', details: '' })
})

test('a saved turn becomes a finished hint', () => {
  const hint = hintFromTurn({ id: 'h', askedAtSec: 61, question: 'Что?', answer: '{"short":"Это.","details":""}', model: 'haiku', createdAt: '', kind: 'hint-hotkey' })
  assert.deepEqual(hint, { id: 'h', kind: 'hint-hotkey', askedAtSec: 61, question: 'Что?', short: 'Это.', details: '', state: 'done' })
})

test('events upsert by id in time order; empty ones are not kept', () => {
  let list = applyHintEvent([], ev('b', 90, 'writing'))
  list = applyHintEvent(list, ev('a', 30))
  list = applyHintEvent(list, ev('b', 90, 'done', { short: 'готово' }))
  assert.deepEqual(list.map((h) => [h.id, h.state, h.short]), [['a', 'done', 's'], ['b', 'done', 'готово']])
  assert.deepEqual(applyHintEvent(list, ev('c', 100, 'empty')), list)
})

test('an empty result removes its own writing card', () => {
  let list = applyHintEvent([], ev('a', 30))
  list = applyHintEvent(list, ev('x', 60, 'writing', { kind: 'hint-hotkey' }))
  list = applyHintEvent(list, ev('x', 60, 'empty', { kind: 'hint-hotkey' }))
  assert.deepEqual(list.map((h) => h.id), ['a'])
})
