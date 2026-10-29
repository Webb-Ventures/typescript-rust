// Deployment failures and overlapping requests: node --test .agents/plugins/code-review/ci/*.test.cjs
'use strict';

const assert = require('node:assert/strict');
const { spawnSync } = require('node:child_process');
const { EventEmitter } = require('node:events');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { test } = require('node:test');
const { promisify } = require('node:util');
const vm = require('node:vm');

const source = fs.readFileSync(require.resolve('../box/dispatcher.mjs'), 'utf8');
const deployCode = source.slice(source.indexOf('const PLUGIN ='), source.indexOf('// Requests.'));
const nextCode = source.slice(source.indexOf('async function next()'), source.indexOf('async function runReview('));
const saveCode = source.slice(source.indexOf('function saveQueue()'), source.indexOf('function restoreQueue()'));
const repo = 'owner/repo';
const shaA = 'a'.repeat(40);
const shaB = 'b'.repeat(40);
const deferred = () => {
  let resolve;
  let reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
};

function dispatcher(stage = async (_, sha) => `deploy/${sha}`) {
  let head = shaA;
  let saved = [];
  const children = [];
  const reviews = [];
  const context = vm.createContext({
    env: { DEPLOY_WAIT_MINUTES: '1' }, DATA: 'data', IMAGE: 'image', QUEUE_FILE: 'queue.json',
    REPOS: [repo], DEPLOY_REPO: repo, DEPLOY_WORKFLOW: '.github/workflows/deploy-box.yml',
    READ: {}, TRUSTED: new Set(['owner']), trust: require('./trust.cjs'),
    split: () => ({ owner: 'owner', name: 'repo' }), short: (sha) => sha.slice(0, 7), label: (job) => job.pr,
    log() {}, setTimeout() {}, promisify, execFile() {}, stage,
    running: null, queue: [{ kind: 'review', pr: 1 }, { kind: 'review', pr: 2 }],
    benchLanes: { slots: [], queue: [] }, writes: Promise.resolve(), fetches: Promise.resolve(),
    fs: {
      readFileSync() { throw new Error('not deployed yet'); },
      statSync: () => ({ gid: 1 }),
      writeFileSync: (_, text) => { saved = JSON.parse(text); },
      renameSync() {},
    },
    spawnSync() {},
    spawn: () => { const child = new EventEmitter(); children.push(child); return child; },
    runReview: (job) => { const done = deferred(); reviews.push({ job, ...done }); return done.promise; },
    startBenches: () => vm.runInContext('saveQueue()', context),
    octokit: async () => ({ rest: {
      actions: { getWorkflowRun: async ({ run_id }) => ({ data: {
        head_sha: run_id === 1 ? shaA : shaB, head_branch: 'main', event: 'push',
        path: '.github/workflows/deploy-box.yml', status: 'in_progress',
        repository: { full_name: repo }, actor: { login: 'owner' }, created_at: new Date().toISOString(),
      } }) },
      repos: {
        get: async () => ({ data: { default_branch: 'main' } }),
        getBranch: async () => ({ data: { commit: { sha: head } } }),
      },
    } }),
  });
  vm.runInContext(`${saveCode}\n${nextCode}\n${deployCode}\nstageDeploy = stage;
    globalThis.box = { next, maybeDeploy, handleDeploy,
      get running() { return running; }, get deploy() { return deploy; } };`, context);
  return { box: context.box, children, reviews, get saved() { return saved; }, setHead: (sha) => { head = sha; } };
}

test('a failed deploy after the drain timeout preserves the active review and persisted queue', async () => {
  const d = dispatcher();
  const first = d.box.next();
  assert.equal((await d.box.handleDeploy({ run_id: 1, repo }))[0], 202);
  d.box.deploy.at -= 60_000;
  await d.box.maybeDeploy();
  assert.equal(d.children.length, 1);
  d.children[0].emit('close', 1);
  assert.equal(d.box.running.pr, 1);
  assert.equal(d.reviews.length, 1);
  assert.deepEqual(d.saved.map((job) => job.pr), [1, 2]);
  d.reviews[0].resolve();
  await first;
  assert.equal(d.box.running.pr, 2);
  assert.equal(d.reviews.length, 2);
});

test('a failed deploy resumes the queue if the active review has finished', async () => {
  const d = dispatcher();
  const first = d.box.next();
  await d.box.handleDeploy({ run_id: 1, repo });
  d.box.deploy.at -= 60_000;
  await d.box.maybeDeploy();
  d.reviews[0].resolve();
  await first;
  assert.equal(d.box.running, null);
  d.children[0].emit('close', 1);
  assert.equal(d.box.running.pr, 2);
  assert.deepEqual(d.saved.map((job) => job.pr), [2]);
});

test('an older staging completion cannot replace a newer queued deployment', async () => {
  const stages = new Map([shaA, shaB].map((sha) => [sha, { ...deferred(), entered: deferred() }]));
  const d = dispatcher((_, sha) => { const s = stages.get(sha); s.entered.resolve(); return s.promise; });
  d.box.next();
  const a = d.box.handleDeploy({ run_id: 1, repo });
  await stages.get(shaA).entered.promise;
  d.setHead(shaB);
  const b = d.box.handleDeploy({ run_id: 2, repo });
  await stages.get(shaB).entered.promise;
  stages.get(shaB).resolve('deploy/b');
  assert.equal((await b)[0], 202);
  const at = d.box.deploy.at;
  stages.get(shaA).resolve('deploy/a');
  assert.equal((await a)[0], 409);
  assert.equal(d.box.deploy.sha, shaB);
  assert.equal(d.box.deploy.src, 'deploy/b');
  assert.equal(d.box.deploy.at, at);
});

test('staging cannot replace a deployment that started while it was fetching', async () => {
  const staged = deferred();
  const entered = deferred();
  const d = dispatcher((_, sha) => {
    if (sha === shaA) return 'deploy/a';
    entered.resolve();
    return staged.promise;
  });
  d.box.next();
  await d.box.handleDeploy({ run_id: 1, repo });
  d.setHead(shaB);
  const b = d.box.handleDeploy({ run_id: 2, repo });
  await entered.promise;
  d.box.deploy.at -= 60_000;
  await d.box.maybeDeploy();
  staged.resolve('deploy/b');
  assert.equal((await b)[0], 409);
  assert.equal(d.box.deploy.sha, shaA);
  assert.equal(d.box.deploy.started, true);
});

test('deployment rolls back failed replacements, but leaves build failures alone', { skip: process.platform === 'win32' }, () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'deploy-test-'));
  try {
    const script = fs.readFileSync(require.resolve('../box/deploy.sh'), 'utf8').replace('DATA=/srv/ts-r-review', 'DATA=$TEST_DATA');
    fs.writeFileSync(path.join(root, 'deploy.sh'), script);
    const mocks = `
docker() {
  echo "$*" >>"$TEST_DATA/calls"
  case "$*" in
    'compose -f new/box/compose.yaml build') [ "$TEST_MODE" != build ] ;;
    'image inspect '*) echo new-image ;;
    'compose -f new/box/compose.yaml up '*) [ "$TEST_MODE" != replace ] ;;
    'inspect '*) echo 'new-image true' ;;
  esac
}
curl() { [ "$TEST_MODE" != health ]; }
sleep() { :; }
script=$1
shift
. "$script"
`;
    for (const mode of ['build', 'replace', 'health', 'success']) {
      const data = path.join(root, mode);
      const previous = path.join(data, 'deploy/old/.agents/plugins/code-review/box');
      fs.mkdirSync(previous, { recursive: true });
      fs.mkdirSync(path.join(data, 'logs'));
      fs.writeFileSync(path.join(previous, 'compose.yaml'), '');
      fs.writeFileSync(path.join(data, 'deploy/current'), 'old\n');
      const result = spawnSync('sh', ['-c', mocks, 'sh', path.join(root, 'deploy.sh'), 'new', 'new-sha'], {
        env: { ...process.env, TEST_DATA: data, TEST_MODE: mode },
        encoding: 'utf8', timeout: 10_000,
      });
      assert.ifError(result.error);
      assert.equal(result.status, mode === 'success' ? 0 : 1, result.stderr);
      assert.ok(fs.existsSync(path.join(data, 'calls')), fs.readFileSync(path.join(data, 'logs/deploy-new-sha.log'), 'utf8'));
      const calls = fs.readFileSync(path.join(data, 'calls'), 'utf8').replaceAll('\\', '/');
      const rollback = mode === 'replace' || mode === 'health';
      assert.equal(calls.includes('tag ts-r-review:previous ts-r-review:latest'), rollback, mode);
      assert.equal(calls.includes(`compose -f ${previous.replaceAll('\\', '/')}/compose.yaml up -d --no-build --remove-orphans`), rollback, mode);
      assert.equal(fs.readFileSync(path.join(data, 'deploy/current'), 'utf8'), mode === 'success' ? 'new-sha\n' : 'old\n');
    }
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});
