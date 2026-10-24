// node --test .github/scripts/ai-policy.test.mjs
import assert from 'node:assert/strict';
import test from 'node:test';
import { DEPENDABOT, attributionFields, findViolations } from './ai-policy.mjs';

const models = (text) => findViolations([{ where: 'description', text }]).map((v) => v.model);

test('denied models are found in any spelling the trailers and tools use', () => {
  for (const text of [
    'Co-authored-by: Gemini <gemini@google.com>',
    'Generated with gemini-2.5-pro',
    'Written with DeepSeek-V3',
    'Used Qwen2.5-Coder for this',
    'qwen3-coder helped',
    'codestral suggestions',
    'via Grok 4',
    'Llama-3.1-70B',
    'Kimi K2 wrote the tests',
    'GLM-4.6',
    'by google-labs-jules[bot]',
  ]) {
    assert.ok(models(text).length > 0, text);
  }
});

test('allowed models and ordinary words pass', () => {
  for (const text of [
    'Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>',
    'Generated with Codex (GPT-6.1)',
    'Drafted with ChatGPT, o3 and Meta Muse',
    'Fixes the gemstone parser, the llamas fixture and a phishing test',
    'Adds mistrals? no: adds the `gramma` rule and `granite` theme',
    'Update emoji tables; quick fix for the qwerty layout',
    '',
  ]) {
    assert.deepEqual(models(text), [], text);
  }
});

test('commits are checked: messages, authors and committers', () => {
  const pull = { title: 'Fix a parser bug', body: 'Small fix.', head: { ref: 'fix/parser' }, user: { login: 'someone' } };
  const commits = [
    { sha: 'abc1234def', commit: { message: 'fix\n\nCo-authored-by: Claude <noreply@anthropic.com>', author: { name: 'A', email: 'a@x' }, committer: { name: 'A', email: 'a@x' } } },
    { sha: 'def5678abc', commit: { message: 'tests', author: { name: 'Gemini Code Assist', email: 'bot@x' }, committer: { name: 'GitHub', email: 'noreply@github.com' } } },
  ];
  const found = findViolations(attributionFields(pull, commits));
  assert.equal(found.length, 1);
  assert.equal(found[0].where, 'commit def5678 author');
});

test("Dependabot's own pull request is skipped, others' commits on it are not", () => {
  const notes = 'Bumps the cargo-minor-patch group with 2 updates.\n\nRelease notes: adds Gemini and Llama providers (#41)';
  const pull = { title: 'Deps: bump the cargo-minor-patch group', body: notes, head: { ref: 'dependabot/cargo/llama-client-2.0.0' }, user: { login: DEPENDABOT } };
  const bot = { sha: 'aaa1111bbb', author: { login: DEPENDABOT }, commit: { message: notes, author: { name: DEPENDABOT, email: '49699333+dependabot[bot]@users.noreply.github.com' }, committer: { name: 'GitHub', email: 'noreply@github.com' } } };
  assert.deepEqual(findViolations(attributionFields(pull, [bot])), []);
  const fixup = { sha: 'ccc2222ddd', author: { login: 'someone' }, commit: { message: 'fix the build\n\nCo-authored-by: Gemini <gemini@google.com>', author: { name: 'A', email: 'a@x' }, committer: { name: 'A', email: 'a@x' } } };
  const found = findViolations(attributionFields(pull, [bot, fixup]));
  assert.deepEqual(found.map((f) => f.where), ['commit ccc2222 message']);
});

test('a commit claiming to be Dependabot is checked on anyone else\'s pull request', () => {
  const pull = { title: 'Bump llama-client', body: 'Uses Llama.', head: { ref: 'dependabot/cargo/llama-client' }, user: { login: 'someone' } };
  const commits = [{ sha: 'eee3333fff', author: { login: DEPENDABOT }, commit: { message: 'Bump llama-client', author: { name: DEPENDABOT, email: 'x' }, committer: { name: 'GitHub', email: 'noreply@github.com' } } }];
  const where = findViolations(attributionFields(pull, commits)).map((f) => f.where);
  assert.deepEqual(where, ['title', 'description', 'branch name', 'commit eee3333 message']);
});
