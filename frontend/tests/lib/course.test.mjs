import test from 'node:test'
import assert from 'node:assert/strict'
import { isSupported, formatTokens, capState, statusLine } from '../../src/lib/course.ts'

test('only the five formats are accepted, case-insensitive', () => {
  assert.equal(isSupported(String.raw`C:\Учёба\Лекция 3.PDF`), true)
  assert.equal(isSupported(String.raw`C:\Учёба.pdf\Лекция`), false)
  assert.equal(isSupported('slides.pptx'), true)
  assert.equal(isSupported('old.doc'), false)
  assert.equal(isSupported('noext'), false)
})

test('tokens read like the mockup', () => {
  assert.equal(formatTokens(9800, 'тыс.', ','), '~9,8 тыс.')
  assert.equal(formatTokens(21000, 'тыс.', ','), '~21 тыс.')
  assert.equal(formatTokens(640, 'тыс.', ','), '~640')
  assert.equal(formatTokens(80000, 'тыс.', ',', false), '80 тыс.')
})

test('cap bar splits materials and summaries and flags overflow', () => {
  assert.deepEqual(capState(38000, 18000, 80000), { percentMaterials: 47.5, percentSummaries: 22.5, over: false })
  const over = capState(102000, 18000, 80000)
  assert.equal(over.over, true)
  assert.equal(over.percentMaterials + over.percentSummaries, 100)
})

test('tokens take unit and decimal separator from the caller', () => {
  assert.equal(formatTokens(9800, 'k', '.'), '~9.8 k')
})

test('status line keeps the dropped tail even when nothing fit', () => {
  const t = (key, v) => key === 'status'
    ? `S:${v.subject}|${v.files}|${v.summaries}|${v.tokens}`
    : key === 'statusDropped' ? `+${v.count}` : key === 'thousands' ? 'k' : ','
  const base = { subject: 'DB', files: 4, summaries: 12, tokens: 56000, droppedFiles: 0, droppedSummaries: 0 }
  assert.equal(statusLine(base, t), 'S:DB|4|12|~56 k')
  assert.equal(statusLine({ ...base, files: 0, summaries: 0, tokens: 0, droppedFiles: 2, droppedSummaries: 3 }, t), 'S:DB|0|0|~0+5')
})
