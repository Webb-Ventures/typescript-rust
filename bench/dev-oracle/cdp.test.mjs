// Unit tests for the shim's page expressions (`node --test bench/dev-oracle/cdp.test.mjs`).
// The expressions run in the browser; here they run against a minimal fake document.

import assert from 'node:assert/strict'
import test from 'node:test'
import { queryExpr } from './cdp.mjs'

function run(expr, elements) {
  const document = { body: { querySelectorAll: () => elements } }
  return new Function('document', `return ${expr}`)(document)
}

const el = (tagName, textContent, children = []) => ({ tagName, textContent, children })

test('text= normalizes whitespace in the page, not only in the selector', () => {
  const inner = el('SPAN', '\n  Good   HTML \n')
  const outer = el('DIV', 'x Good HTML', [inner])
  assert.equal(run(queryExpr('text=good html'), [outer, inner]), inner)
  const status = el('DIV', 'status')
  assert.equal(run(queryExpr('text=status'), [status]), status)
  assert.equal(run(queryExpr('text=missing'), [status]), null)
})
