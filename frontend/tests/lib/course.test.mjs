import test from 'node:test'
import assert from 'node:assert/strict'
import { isSupported, formatTokens, capState, statusLine, fileBadge, fileWarning } from '../../src/lib/course.ts'

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

test('a row being read shows plain reading, then N of M', () => {
  const reading = { state: 'reading', pagesWaiting: 0, error: null }
  assert.equal(fileBadge(reading).key, 'stateReadingPlain')
  assert.equal(fileBadge(reading, { done: 0, total: 0 }).key, 'stateReadingPlain')
  assert.deepEqual(fileBadge(reading, { done: 2, total: 5 }).values, { done: 2, total: 5 })
})

test('picture reading shows N of M on a waiting row until it is done', () => {
  const waits = { state: 'waits-claude', pagesWaiting: 30, error: null }
  assert.deepEqual(fileBadge(waits), { key: 'stateWaits', values: { count: 30 }, tone: 'waits' })
  const going = fileBadge(waits, { done: 3, total: 30 })
  assert.equal(going.key, 'stateReading')
  assert.deepEqual(going.values, { done: 3, total: 30 })
  assert.equal(fileBadge(waits, { done: 30, total: 30 }).key, 'stateWaits')
})

test('a non-error row with a reason shows it as a warning', () => {
  assert.equal(fileWarning({ state: 'ready', error: 'Не удалось прочитать файл' }), 'Не удалось прочитать файл')
  assert.equal(fileWarning({ state: 'waits-claude', error: 'обезличивание включено — картинки не отправляются' }), 'обезличивание включено — картинки не отправляются')
  assert.equal(fileWarning({ state: 'error', error: 'x' }), null)
  assert.equal(fileWarning({ state: 'ready', error: null }), null)
  assert.equal(fileBadge({ state: 'error', pagesWaiting: 0, error: 'x' }).key, 'stateError')
})
