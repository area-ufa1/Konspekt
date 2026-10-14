import test from 'node:test'
import assert from 'node:assert/strict'
import { isSupported, formatTokens, capState } from '../../src/lib/course.ts'

test('only the five formats are accepted, case-insensitive', () => {
  assert.equal(isSupported('C:\Учёба\Лекция 3.PDF'), true)
  assert.equal(isSupported('slides.pptx'), true)
  assert.equal(isSupported('old.doc'), false)
  assert.equal(isSupported('noext'), false)
})

test('tokens read like the mockup', () => {
  assert.equal(formatTokens(9800), '~9,8 тыс.')
  assert.equal(formatTokens(21000), '~21 тыс.')
  assert.equal(formatTokens(640), '~640')
  assert.equal(formatTokens(80000, false), '80 тыс.')
})

test('cap bar splits materials and summaries and flags overflow', () => {
  assert.deepEqual(capState(38000, 18000, 80000), { percentMaterials: 47.5, percentSummaries: 22.5, over: false })
  const over = capState(102000, 18000, 80000)
  assert.equal(over.over, true)
  assert.equal(over.percentMaterials + over.percentSummaries, 100)
})
