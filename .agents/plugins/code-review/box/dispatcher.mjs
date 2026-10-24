// The review box's dispatcher: the only long-running service. The code-review and bench
// workflows join the tailnet, POST { run_id, pr, comment_id, repo } here and exit. One box serves
// every repository in REPOS (the private repo and its public mirror). The dispatcher checks the
// request against GitHub's own record of that workflow run in that repository (workflow, event,
// triggering user; the rules are in ci/trust.cjs), queues the work, keeps a progress line in the
// PR's summary or bench comment, and runs each step in a fresh container (see job.mjs). Codex
// jobs and benches are two lanes on separate CPU cores (REVIEW_CPUS, and the bench slots) so a
// bench measures on cores nothing else uses: Codex jobs one at a time, benches one per slot
// (BENCH_SLOTS; one slot on BENCH_CPUS by default, see ci/bench-lanes.cjs). It acts on GitHub
// as the ts-r review GitHub App, minting a short-lived installation token for every step, scoped
// to the job's repository and the step's permissions.
import { createRequire } from 'node:module';
import { spawn, spawnSync } from 'node:child_process';
import crypto from 'node:crypto';
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';

const require = createRequire(import.meta.url);
const review = require('./ci/github.cjs');
const benchComment = require('./ci/bench.cjs');
const trust = require('./ci/trust.cjs');
const benchLanesLib = require('./ci/bench-lanes.cjs');
const { Octokit } = await import('octokit');

const env = process.env;
// The repositories served; requests without a repo are for the first. Jobs carry their repo.
const REPOS = trust.parseRepos(env.REPOS, env.REPO);
const split = (repo) => {
  const [owner, name] = repo.split('/');
  return { owner, name };
};
const DATA = env.DATA_DIR ?? '/srv/ts-r-review';
const IMAGE = env.IMAGE ?? 'ts-r-review:latest';
const TRUSTED = new Set(JSON.parse(env.TRUSTED_USERS ?? '["riley-wv"]'));
const WORKFLOW = '.github/workflows/code-review.yml';
const BENCH_WORKFLOW = '.github/workflows/bench.yml';
// The bench's public repos (bench/repos.toml): small and medium ones, a few minutes each.
const BENCH_REPOS = env.BENCH_REPOS ?? 'trpc,date-fns,rxjs,three-x10';
// Each repo has its own bench directory (checkouts, crates, package stores, passes), so code
// from one repo's PR never sees another repo's git objects or skews its benches.
const benchDir = (repo) => `${DATA}/bench/${repo.replace('/', '__')}`;
// The lanes this box runs (LANES, default both). A separate bench box serves ["bench"] and the
// review box ["review"], so a request sent to the wrong one is refused rather than run there.
const LANES = new Set(JSON.parse(env.LANES ?? '["review","bench"]'));
// The box has 6 cores: Codex jobs (mostly waiting on the model) get two, benches the other four.
const REVIEW_CPUS = ['--cpuset-cpus', env.REVIEW_CPUS ?? '0-1'];
// Bench slots: each runs one bench at a time, the PR and its base both, on a cpuset of its own.
// Without BENCH_SLOTS there is one, on BENCH_CPUS (default 2-5). With BENCH_SLOTS=auto (or a
// count) they are sized from the cores Docker reports, BENCH_SLOT_CPUS (default 4) each, after
// REVIEW_CPUS when this box runs reviews; or BENCH_SLOTS lists the cpusets. A bad setting falls
// back to the one default slot, logged at startup and shown on /healthz.
const HOST_CORES = (() => {
  const result = spawnSync('docker', ['info', '--format', '{{.NCPU}}'], { encoding: 'utf8', timeout: 20_000 });
  const cores = Number(result.stdout?.trim());
  return Number.isInteger(cores) && cores > 0 ? cores : null;
})();
const SLOTS = benchLanesLib.benchSlots(
  { slots: env.BENCH_SLOTS, cpus: env.BENCH_CPUS, slotCpus: env.BENCH_SLOT_CPUS, reviewCpus: env.REVIEW_CPUS, review: LANES.has('review') },
  HOST_CORES,
);
// BENCH_MEASURE=alone (the default): a slot measures only while no other slot builds, installs
// or measures, so its numbers come from a box as quiet as with one slot, and only the builds
// overlap. BENCH_MEASURE=parallel: slots measure side by side, kept apart only by their cores
// (they still share the memory bus, the last-level cache and the disk; the confirming re-run,
// PR first, has to see a regression again). With one slot the two are the same.
const BENCH_MEASURE = env.BENCH_MEASURE === 'parallel' ? 'parallel' : 'alone';
// The memory of each bench container (BENCH_MEMORY, default 6g): slots times this must fit.
const BENCH_MEMORY = /^\d+[kmg]?$/i.test(env.BENCH_MEMORY ?? '') ? env.BENCH_MEMORY : '6g';
// Reference tools (tsc, ESLint, esbuild, ...) measured on a slot are reused for this many days
// (BENCH_REFERENCE_DAYS, default 7; 0 measures them every time) while nothing that could change
// them changed: see referenceKey.
const REFERENCE_DAYS = Number.isFinite(Number(env.BENCH_REFERENCE_DAYS ?? 7)) ? Number(env.BENCH_REFERENCE_DAYS ?? 7) : 7;
// The job image's id is part of the reference key (it carries Node and hyperfine).
const IMAGE_ID = (() => {
  const result = spawnSync('docker', ['image', 'inspect', '--format', '{{.Id}}', IMAGE], { encoding: 'utf8', timeout: 20_000 });
  return result.status === 0 ? result.stdout.trim() : null;
})();
const MODEL = env.MODEL ?? 'gpt-6.1-sol';
const EFFORT = env.EFFORT ?? 'high';
for (const file of [env.GITHUB_APP_KEY_PATH ?? `${DATA}/secrets/github-app.pem`, env.DISPATCH_TOKEN_PATH ?? `${DATA}/secrets/dispatch-token`]) {
  if (!fs.existsSync(file)) {
    console.error(`Missing ${file}; see the README's box setup. Retrying after a restart.`);
    await new Promise((r) => setTimeout(r, 60_000));
    process.exit(1);
  }
}
if (!env.GITHUB_APP_ID) {
  console.error('GITHUB_APP_ID is not set in config.env.');
  await new Promise((r) => setTimeout(r, 60_000));
  process.exit(1);
}
const APP_KEY = fs.readFileSync(env.GITHUB_APP_KEY_PATH ?? `${DATA}/secrets/github-app.pem`, 'utf8');
const DISPATCH_TOKEN = fs.readFileSync(env.DISPATCH_TOKEN_PATH ?? `${DATA}/secrets/dispatch-token`, 'utf8').trim();

const log = (...args) => console.log(new Date().toISOString(), ...args);
const short = (sha) => sha.slice(0, 7);
const utc = () => new Date().toISOString().slice(11, 16) + ' UTC';

// GitHub App auth: a JWT for the app, then installation tokens scoped per step.
const b64url = (data) => Buffer.from(data).toString('base64url');
function appJwt() {
  const now = Math.floor(Date.now() / 1000);
  const unsigned = `${b64url(JSON.stringify({ alg: 'RS256', typ: 'JWT' }))}.${b64url(JSON.stringify({ iat: now - 60, exp: now + 540, iss: env.GITHUB_APP_ID }))}`;
  return `${unsigned}.${crypto.sign('RSA-SHA256', Buffer.from(unsigned), APP_KEY).toString('base64url')}`;
}
// Installation ids per repository. GITHUB_APP_INSTALLATION_ID is the installation on the first
// repository's account, so it holds for every repository of that account.
const installations = new Map();
async function token(repo, permissions) {
  const { owner, name } = split(repo);
  const app = new Octokit({ auth: appJwt() });
  if (!installations.has(repo)) {
    const id = env.GITHUB_APP_INSTALLATION_ID && owner === split(REPOS[0]).owner
      ? env.GITHUB_APP_INSTALLATION_ID
      : (await app.rest.apps.getRepoInstallation({ owner, repo: name })).data.id;
    installations.set(repo, id);
  }
  const { data } = await app.rest.apps.createInstallationAccessToken({ installation_id: installations.get(repo), repositories: [name], permissions });
  return data.token;
}
const READ = { contents: 'read', pull_requests: 'read', issues: 'read', actions: 'read' };
// Contents write: GitHub only lets an app resolve review threads with it.
const WRITE = { contents: 'write', pull_requests: 'write', issues: 'write' };
// The App's bot user, which fix commits are authored by.
let bot = null;
async function botUser() {
  if (!bot) {
    const { data: app } = await new Octokit({ auth: appJwt() }).rest.apps.getAuthenticated();
    const login = `${app.slug}[bot]`;
    const { data: user } = await new Octokit({ auth: await token(REPOS[0], READ) }).rest.users.getByUsername({ username: login });
    bot = { login, email: `${user.id}+${login}@users.noreply.github.com` };
  }
  return bot;
}
const octokit = async (repo, permissions) => new Octokit({ auth: await token(repo, permissions) });
const contextFor = (repo) => ({ repo: { owner: split(repo).owner, repo: split(repo).name }, serverUrl: 'https://github.com', payload: {} });
// Each repository's bare clone. The first repository's used to be cache/repo.git, which is moved
// at startup.
const cacheDir = (repo) => `${DATA}/cache/${repo.replace('/', '__')}.git`;
// Job ids name directories, logs and containers: the repository, the PR and the time.
const jobId = (repo, pr) => `${split(repo).name}-${pr}-${Date.now()}`;
const label = (job) => `${job.kind} ${job.repo}#${job.pr}`;
const samePr = (a, b) => a.repo === b.repo && a.pr === b.pr;

// Containers.
function docker(args, { name, envs: given = {}, timeoutMs, logFile }) {
  const envs = Object.fromEntries(Object.entries(given).filter(([, v]) => v !== undefined && v !== null && v !== ''));
  return new Promise((resolve) => {
    const out = fs.openSync(logFile, 'a');
    fs.writeSync(out, `\n=== ${new Date().toISOString()} ${name}\n`);
    const child = spawn('docker', ['run', '--rm', '--name', name, ...Object.keys(envs).flatMap((k) => ['-e', k]), ...args], {
      env: { ...process.env, ...envs }, stdio: ['ignore', out, out],
    });
    const timer = setTimeout(() => {
      fs.writeSync(out, `\n=== timed out after ${timeoutMs / 60000} min\n`);
      spawn('docker', ['rm', '-f', name], { stdio: 'ignore' });
    }, timeoutMs);
    child.on('close', (code) => {
      clearTimeout(timer);
      fs.closeSync(out);
      resolve(code ?? 1);
    });
  });
}
// The Codex containers hold the ChatGPT login and reach OpenAI, so no code from a PR may run in
// them: the toolchain and npm are hidden, and a fix job's builds and tests go to its runner.
const NO_TOOLCHAIN = ['/usr/local/cargo', '/usr/local/rustup', '/usr/local/lib/node_modules/npm', '/usr/local/lib/node_modules/corepack']
  .flatMap((dir) => ['--mount', `type=tmpfs,dst=${dir},tmpfs-mode=0555`]);
const LOCKED_DOWN = ['--read-only', '--tmpfs', '/tmp:rw,size=1g', '--cap-drop', 'ALL', '--security-opt', 'no-new-privileges', '--user', '1000:1000', '--pids-limit', '512', '-e', 'HOME=/tmp'];

// Codex jobs (reviews and fixes) run one at a time (one Codex login, one stream); a newer
// request of the same kind for the same PR replaces a queued one. Everything that writes to GitHub goes through one chain so the
// summary comment is never edited by two steps at once.
const queue = [];
let running = null;
let writes = Promise.resolve();
const serialize = (fn) => (writes = writes.then(fn, fn).catch((error) => log('github step failed:', error.message)));
// The repo cache is fetched by one prepare step at a time (the two lanes would race on its refs).
let fetches = Promise.resolve();
const withCache = (fn) => (fetches = fetches.then(fn, fn));

// Both lanes' queues, with the running jobs first, survive a restart (a redeploy): the running
// jobs start again from the beginning.
const QUEUE_FILE = `${DATA}/queue.json`;
function saveQueue() {
  const strip = ({ cancelled, container, abort, ...job }) => job;
  // A bench being stopped is moot: it is not started again.
  const benches = [...benchLanes.slots.map((slot) => slot.job).filter((job) => job && !job.cancelled), ...benchLanes.queue];
  const jobs = [running, ...queue, ...benches].filter(Boolean).map(strip);
  fs.writeFileSync(`${QUEUE_FILE}.new`, JSON.stringify(jobs, null, 2));
  fs.renameSync(`${QUEUE_FILE}.new`, QUEUE_FILE);
}
function restoreQueue() {
  let jobs = [];
  try {
    jobs = JSON.parse(fs.readFileSync(QUEUE_FILE, 'utf8'));
  } catch {
    return;
  }
  // Entries from before the box served several repositories are the first repository's.
  jobs = jobs.map((job) => ({ ...job, repo: job.repo ?? REPOS[0] })).filter((job) => {
    if (!REPOS.includes(job.repo)) {
      log(`dropping queued ${label(job)}: ${job.repo} is no longer in REPOS`);
      return false;
    }
    if (!LANES.has(job.kind === 'bench' ? 'bench' : 'review')) {
      log(`dropping queued ${label(job)}: this box no longer runs its lane (LANES)`);
      return false;
    }
    return true;
  });
  for (const job of jobs) (job.kind === 'bench' ? benchLanes.queue : queue).push(job);
  log(`restored ${jobs.length} queued jobs: ${jobs.map(label).join(' ')}`);
  next();
  startBenches();
}

async function status(repo, pr, text, { fresh = false, busy = false } = {}) {
  await serialize(async () => {
    process.env.PR_NUMBER = String(pr);
    await review.setStatus({ github: await octokit(repo, WRITE), context: contextFor(repo), status: text, fresh, busy });
  });
}

function enqueue(job) {
  const existing = queue.findIndex((j) => samePr(j, job) && j.kind === job.kind);
  if (existing >= 0) queue.splice(existing, 1);
  queue.push(job);
  const ahead = queue.length - 1 + (running ? 1 : 0);
  const behind = ahead ? ` behind ${ahead} other job${ahead === 1 ? '' : 's'}` : '';
  if (job.kind === 'fix') status(job.repo, job.pr, `Fixing the open findings with Codex: queued${behind} (requested by @${job.by}).`, { busy: true });
  else status(job.repo, job.pr, `Review of \`${short(job.head)}\` queued${behind} (requested by @${job.by}).`, { fresh: true, busy: true });
  saveQueue();
  if (!running) next();
}
const enqueueReview = (job) => enqueue({ ...job, kind: 'review' });

async function next() {
  running = queue.shift() ?? null;
  saveQueue();
  if (!running) return;
  try {
    await (running.kind === 'fix' ? runFix(running) : runReview(running));
  } catch (error) {
    log(`${label(running)} crashed:`, error.stack);
  }
  next();
}

async function runReview({ repo, pr, head, by, runUrl }) {
  const id = jobId(repo, pr);
  const dir = `${DATA}/jobs/${id}`;
  const logFile = `${DATA}/logs/${id}.log`;
  for (const sub of ['in', 'out']) fs.mkdirSync(`${dir}/${sub}`, { recursive: true });
  const base = { REPO: repo, PR_NUMBER: String(pr), JOB_DIR: dir, CACHE_DIR: cacheDir(repo) };
  log(`review ${repo}#${pr} at ${short(head)} for @${by}: ${logFile}`);
  const started = new Date().toISOString();
  await status(repo, pr, `Review of \`${short(head)}\` running since ${utc()} (requested by @${by}).`, { busy: true });

  const prepared = await withCache(async () => docker(['--network', 'bridge', ...LOCKED_DOWN, '-v', `${dir}:${dir}`, '-v', `${cacheDir(repo)}:${cacheDir(repo)}`, IMAGE, 'node', '/app/job.mjs', 'prepare'], {
    name: `tsr-prepare-${id}`, envs: { ...base, GH_TOKEN: await token(repo, READ) }, timeoutMs: 10 * 60_000, logFile,
  }));
  let outcome = 'failure';
  const login = loginSecrets();
  if (!login) fs.appendFileSync(logFile, '\n=== the Codex login cannot be read; not running\n');
  if (prepared === 0 && login) {
    const reviewed = await docker([
      '--network', env.EGRESS_NETWORK ?? 'ts-r-review-egress', ...LOCKED_DOWN, ...NO_TOOLCHAIN,
      '--memory', '6g', ...REVIEW_CPUS,
      '-e', 'HTTPS_PROXY=http://egress:3128', '-e', 'HTTP_PROXY=http://egress:3128', '-e', 'CODEX_HOME=/codex',
      '-v', `${DATA}/codex-home:/codex`,
      '-v', `${dir}/in:${dir}/in:ro`, '-v', `${dir}/repo:${dir}/repo:ro`, '-v', `${dir}/out:${dir}/out`,
      '-v', `${cacheDir(repo)}:${cacheDir(repo)}:ro`,
      IMAGE, 'node', '/app/job.mjs', 'codex',
    ], { name: `tsr-review-${id}`, envs: { ...base, MODEL, EFFORT }, timeoutMs: 45 * 60_000, logFile });
    if (reviewed === 0 && credentialsAbsent([`${dir}/out/result.json`], logFile, login)) outcome = 'success';

    // Posting is retried once (GitHub hiccups); if it still fails the findings are kept in
    // the logs and the summary says so, rather than being lost with the job directory.
    let published = 1;
    for (let attempt = 1; attempt <= 2 && published !== 0; attempt += 1) {
      if (attempt > 1) await new Promise((r) => setTimeout(r, 30_000));
      published = await serialize(async () => docker(['--network', 'bridge', ...LOCKED_DOWN, '-v', `${dir}:${dir}:ro`, IMAGE, 'node', '/app/job.mjs', 'publish'], {
        name: `tsr-publish-${id}-${attempt}`, logFile, timeoutMs: 5 * 60_000,
        envs: { ...base, GH_TOKEN: await token(repo, WRITE), REVIEW_OUTCOME: outcome, MODEL, EFFORT, MIN_CONFIDENCE: '80', RUN_URL: runUrl, REVIEW_STARTED: started },
      }));
    }
    if (published !== 0 && outcome === 'success') {
      outcome = 'unpublished';
      fs.copyFileSync(`${dir}/out/result.json`, `${DATA}/logs/${id}.findings.json`);
      await status(repo, pr, `The review of \`${short(head)}\` finished but posting it failed; its findings are saved on the review box (\`logs/${id}.findings.json\`). Comment \`@autoreview\` to retry.`);
    }
  } else {
    await status(repo, pr, `The review of \`${short(head)}\` could not start: preparing the checkout failed. Comment \`@autoreview\` to retry.`);
  }
  fs.rmSync(dir, { recursive: true, force: true });
  pruneLogs();
  log(`review ${repo}#${pr} done: ${outcome}`);
}

// Codex fixes the open findings on the PR branch; the commit step commits its edits, and the
// push step pushes them and replies on and resolves each thread.
async function runFix({ repo, pr, by, runUrl }) {
  const id = jobId(repo, pr);
  const dir = `${DATA}/jobs/${id}`;
  const logFile = `${DATA}/logs/${id}-fix.log`;
  for (const sub of ['in', 'out']) fs.mkdirSync(`${dir}/${sub}`, { recursive: true });
  const base = { REPO: repo, PR_NUMBER: String(pr), JOB_DIR: dir, CACHE_DIR: cacheDir(repo), MODE: 'fix' };
  log(`fix ${repo}#${pr} for @${by}: ${logFile}`);
  const started = new Date().toISOString();
  await status(repo, pr, `Codex is fixing the open findings, running since ${utc()} (requested by @${by}).`, { busy: true });
  const { login, email } = await botUser();

  const prepared = await withCache(async () => docker(['--network', 'bridge', ...LOCKED_DOWN, '-v', `${dir}:${dir}`, '-v', `${cacheDir(repo)}:${cacheDir(repo)}`, IMAGE, 'node', '/app/job.mjs', 'prepare'], {
    name: `tsr-prepare-${id}`, envs: { ...base, GH_TOKEN: await token(repo, READ) }, timeoutMs: 10 * 60_000, logFile,
  }));
  let outcome = 'failure';
  const secrets = loginSecrets();
  if (!secrets) fs.appendFileSync(logFile, '\n=== the Codex login cannot be read; not running\n');
  if (prepared === 0 && secrets) {
    // Dependencies are installed before Codex runs, by a container with no Codex login and no
    // GitHub token, through the registry allowlist, into this job's own directories. Codex then
    // works offline: a tunnel to a registry also carries authenticated uploads, so the container
    // holding the login must not reach one, and nothing one job installs is shared with another.
    fs.mkdirSync(`${dir}/deps`);
    await docker(['--network', 'ts-r-review-egress-deps', ...LOCKED_DOWN, '--pids-limit', '4096',
      ...['HTTPS_PROXY', 'HTTP_PROXY', 'https_proxy', 'http_proxy'].flatMap((name) => ['-e', `${name}=http://egress-deps:3128`]),
      '-v', `${dir}/git:${dir}/git:ro`, '-v', `${dir}/repo:${dir}/repo`, '-v', `${dir}/deps:${dir}/deps`, '-v', `${cacheDir(repo)}:${cacheDir(repo)}:ro`,
      '-e', `CARGO_HOME=${dir}/deps/cargo`, '-e', `npm_config_cache=${dir}/deps/npm`,
      IMAGE, 'node', '/app/job.mjs', 'deps',
    ], { name: `tsr-deps-${id}`, envs: base, timeoutMs: 15 * 60_000, logFile });
    // The job's build directory; deleted when the job ends (disk on the box is tight).
    const target = `${DATA}/targets/${id}`;
    fs.mkdirSync(target, { recursive: true });
    // Builds, tests and package scripts run in the runner, which Codex reaches through box-run
    // (a directory both mount): no network, no login, nothing to steal and nowhere to send it.
    fs.mkdirSync(`${dir}/run`);
    const runner = docker(['--network', 'none', ...LOCKED_DOWN, '--memory', '6g', '--cpus', '4', '--pids-limit', '4096',
      '-v', `${dir}/run:${dir}/run`, '-v', `${dir}/git:${dir}/git:ro`, '-v', `${dir}/repo:${dir}/repo`, '-v', `${cacheDir(repo)}:${cacheDir(repo)}:ro`,
      '-v', `${dir}/deps:${dir}/deps:ro`, '-e', `CARGO_HOME=${dir}/deps/cargo`, '-e', 'CARGO_NET_OFFLINE=true',
      '-e', `npm_config_cache=${dir}/deps/npm`, '-e', 'npm_config_offline=true',
      '-v', `${target}:/target`, '-e', 'CARGO_TARGET_DIR=/target', '-e', 'CARGO_BUILD_JOBS=4',
      // Smaller build directories: line tables only, no incremental cache.
      '-e', 'CARGO_PROFILE_DEV_DEBUG=line-tables-only', '-e', 'CARGO_PROFILE_TEST_DEBUG=line-tables-only', '-e', 'CARGO_INCREMENTAL=0',
      IMAGE, 'node', '/app/job.mjs', 'runner',
    ], { name: `tsr-run-${id}`, envs: { ...base, RUN_DIR: `${dir}/run` }, timeoutMs: 50 * 60_000, logFile });
    const fixed = await docker([
      // OpenAI only, like a review. No GitHub token, no toolchain, no installed dependencies.
      '--network', env.EGRESS_NETWORK ?? 'ts-r-review-egress', ...LOCKED_DOWN, ...NO_TOOLCHAIN, ...hiddenNodeModules(dir),
      '--memory', '2g', ...REVIEW_CPUS,
      '-e', 'HTTPS_PROXY=http://egress:3128', '-e', 'HTTP_PROXY=http://egress:3128', '-e', 'CODEX_HOME=/codex',
      '-v', `${DATA}/codex-home:/codex`,
      '-v', `${dir}/run:${dir}/run`, '-e', `RUN_DIR=${dir}/run`,
      '-v', `${dir}/in:${dir}/in:ro`, '-v', `${dir}/git:${dir}/git:ro`, '-v', `${dir}/repo:${dir}/repo`, '-v', `${dir}/out:${dir}/out`,
      '-v', `${cacheDir(repo)}:${cacheDir(repo)}:ro`,
      IMAGE, 'node', '/app/job.mjs', 'codex',
    ], { name: `tsr-fix-${id}`, envs: { ...base, MODEL, EFFORT }, timeoutMs: 45 * 60_000, logFile });
    spawnSync('docker', ['rm', '-f', `tsr-run-${id}`], { stdio: 'ignore' });
    await runner;
    if (fixed === 0 && credentialsAbsent([`${dir}/out/result.json`], logFile, secrets)) {
      const committed = await docker(['--network', 'none', ...LOCKED_DOWN,
        '-v', `${dir}/in:${dir}/in:ro`, '-v', `${dir}/git:${dir}/git`, '-v', `${dir}/repo:${dir}/repo:ro`, '-v', `${dir}/out:${dir}/out`,
        '-v', `${cacheDir(repo)}:${cacheDir(repo)}:ro`,
        IMAGE, 'node', '/app/job.mjs', 'commit',
      ], { name: `tsr-commit-${id}`, envs: { ...base, ACTOR: by, BOT_NAME: login, BOT_EMAIL: email }, timeoutMs: 5 * 60_000, logFile });
      if (committed === 0 && credentialsAbsent([`${dir}/out/commit.json`, `${dir}/out/fix.patch`, `${dir}/out/blobs.bin`], logFile, secrets)) outcome = 'success';
    }
  }
  // The push step exits 0 when it pushed and replied, 3 when GitHub refused the push (the summary
  // says why), anything else when it broke part-way; undefined means no token, so it never ran.
  // Only that last case is retried: once the step has pushed or replied, a rerun would reply twice.
  let pushed;
  for (let attempt = 1; attempt <= 2 && pushed === undefined; attempt += 1) {
    if (attempt > 1) await new Promise((r) => setTimeout(r, 30_000));
    pushed = await serialize(async () => docker(['--network', 'bridge', ...LOCKED_DOWN, '-v', `${dir}:${dir}:ro`, '-v', `${cacheDir(repo)}:${cacheDir(repo)}:ro`, IMAGE, 'node', '/app/job.mjs', 'push'], {
      name: `tsr-push-${id}-${attempt}`, logFile, timeoutMs: 5 * 60_000,
      envs: { ...base, GH_TOKEN: await token(repo, WRITE), FIX_OUTCOME: outcome, ACTOR: by, RUN_URL: runUrl, REVIEW_STARTED: started, BOT_NAME: login, BOT_EMAIL: email },
    }));
  }
  if (outcome === 'success' && pushed !== 0) {
    // Keep the fix rather than deleting it with the job directory.
    for (const file of ['commit.json', 'fix.patch', 'result.json']) fs.copyFileSync(`${dir}/out/${file}`, `${DATA}/logs/${id}-fix.${file}`);
    outcome = pushed === 3 ? 'push refused' : 'unpublished';
    if (pushed !== 3) await status(repo, pr, `Codex finished fixing the findings for @${by}, but pushing or replying failed part-way; the fix is saved on the review box (\`logs/${id}-fix.*\`). Check the branch and the threads, then tick the box again to retry.`);
  } else if (pushed === undefined) {
    await status(repo, pr, `The fix run for @${by} did not complete. Nothing was changed.`);
  }
  fs.rmSync(dir, { recursive: true, force: true });
  fs.rmSync(`${DATA}/targets/${id}`, { recursive: true, force: true });
  pruneLogs();
  log(`fix ${repo}#${pr} done: ${outcome}`);
}

// tmpfs mounts over the node_modules the deps step installed (it lists them), so the Codex
// container cannot run package code either. Only plain paths that resolve inside the checkout.
function hiddenNodeModules(dir) {
  let listed = [];
  try {
    listed = JSON.parse(fs.readFileSync(`${dir}/deps/node-modules.json`, 'utf8'));
  } catch {
    return [];
  }
  const repo = `${dir}/repo`;
  return listed.filter((rel) => typeof rel === 'string' && /^[\w@.+-]+(\/[\w@.+-]+)*$/.test(rel) && !rel.split('/').includes('..'))
    .map((rel) => path.join(repo, rel))
    .filter((abs) => {
      try {
        return fs.realpathSync(abs) === abs && fs.statSync(abs).isDirectory();
      } catch {
        return false;
      }
    })
    .flatMap((abs) => ['--mount', `type=tmpfs,dst=${abs},tmpfs-mode=0555`]);
}

// The strings in the Codex login (tokens, ids), or null if it cannot be read. Taken before a
// Codex container starts, because that container can rewrite the file.
function loginSecrets() {
  const secrets = [];
  const walk = (v) => (typeof v === 'string' ? v.length >= 16 && secrets.push(v) : v && typeof v === 'object' && Object.values(v).forEach(walk));
  try {
    walk(JSON.parse(fs.readFileSync(`${DATA}/codex-home/auth.json`, 'utf8')));
  } catch {
    return null;
  }
  return secrets.length > 0 ? secrets : null;
}

// Refuse to post or push output that contains any token from the Codex login: the login as it
// was before Codex ran (`before`) and as it is now (Codex may have refreshed it). Files are read
// as raw bytes. Missing files count as clean only when optional (fix.patch and blobs.bin are
// absent when nothing changed).
function credentialsAbsent(files, logFile, before) {
  if (!fs.existsSync(files[0]) || !before) return false;
  const text = files.filter((f) => fs.existsSync(f)).map((f) => fs.readFileSync(f, 'latin1')).join('\n');
  const secrets = [...before, ...(loginSecrets() ?? [])];
  if (secrets.some((s) => text.includes(s))) {
    fs.appendFileSync(logFile, '\n=== output contains Codex credentials; not posting\n');
    return false;
  }
  return true;
}

// A bench leaves only what saves downloads: the slot's checkouts' sources, the pinned repos
// (target/bench-repos), the TypeScript lib files (target/upstream), crates and package stores.
// Builds, bench output and results go when it ends, unless BENCH_KEEP_BUILDS=1 (a box with the
// disk for them): then the next run's base build is a no-op when the base is unchanged (the
// checkout leaves unchanged files' mtimes alone) and its PR build recompiles only what changed.
// That is why a slot goes back to the PR, or else the base, it ran last (ci/bench-lanes.cjs).
const BENCH_KEPT = new Set(['bench-repos', 'upstream']);
const KEEP_BUILDS = env.BENCH_KEEP_BUILDS === '1';
// Steps that run PR code can write the checkouts, so a path in one may lead through a link. This
// removes `rel` under `root` without following one: a link on the way is removed instead.
function removeUnder(root, rel) {
  let at = root;
  for (const part of rel.split('/')) {
    at = `${at}/${part}`;
    let stat;
    try {
      stat = fs.lstatSync(at);
    } catch {
      return;
    }
    if (!stat.isDirectory() || stat.isSymbolicLink()) {
      if (at !== `${root}/${rel}` && !stat.isSymbolicLink()) return;
      break;
    }
  }
  fs.rmSync(at, { recursive: true, force: true });
}
// The entries of a directory under `root`, or none if it is not a plain directory there.
function entriesUnder(root, rel) {
  let at = root;
  for (const part of rel.split('/')) {
    at = `${at}/${part}`;
    try {
      const stat = fs.lstatSync(at);
      if (!stat.isDirectory() || stat.isSymbolicLink()) return [];
    } catch {
      return [];
    }
  }
  return fs.readdirSync(at);
}
function cleanBench(slot) {
  for (const name of ['base', 'pr']) {
    if (!KEEP_BUILDS) for (const entry of entriesUnder(slot, `${name}/target`)) if (!BENCH_KEPT.has(entry)) removeUnder(slot, `${name}/target/${entry}`);
    removeUnder(slot, `${name}/bench/results`);
  }
}
// A file a job step wrote, or null: never through a link, never a FIFO or device (which could
// block the dispatcher or read something else), and at most `max` bytes.
function readOutFile(file, max) {
  let fd;
  try {
    fd = fs.openSync(file, fs.constants.O_RDONLY | (fs.constants.O_NOFOLLOW ?? 0) | (fs.constants.O_NONBLOCK ?? 0));
    const stat = fs.fstatSync(fd);
    if (!stat.isFile() || stat.size > max) return null;
    return fs.readFileSync(fd, 'utf8');
  } catch {
    return null;
  } finally {
    if (fd !== undefined) fs.closeSync(fd);
  }
}

// The last 200 logs, oldest out first (names start with the repository, so not by name).
function pruneLogs() {
  const mtime = (file) => {
    try {
      return fs.statSync(`${DATA}/logs/${file}`).mtimeMs;
    } catch {
      return 0;
    }
  };
  const files = fs.readdirSync(`${DATA}/logs`).map((file) => [mtime(file), file])
    .sort(([a, x], [b, y]) => a - b || x.localeCompare(y)).map(([, file]) => file);
  for (const file of files.slice(0, Math.max(0, files.length - 200))) fs.rmSync(`${DATA}/logs/${file}`, { force: true });
}

function runResolve({ repo, pr, event, action, commentId, actor }) {
  const id = jobId(repo, pr);
  serialize(async () => {
    const code = await docker(['--network', 'bridge', ...LOCKED_DOWN, IMAGE, 'node', '/app/job.mjs', 'resolve'], {
      name: `tsr-resolve-${id}`, logFile: `${DATA}/logs/${id}-resolve.log`, timeoutMs: 5 * 60_000,
      envs: { REPO: repo, PR_NUMBER: String(pr), EVENT: event, ACTION: action ?? '', COMMENT_ID: commentId ? String(commentId) : '', ACTOR: actor ?? '', GH_TOKEN: await token(repo, WRITE) },
    });
    log(`${event} ${repo}#${pr}${action ? ` ${action}` : ''}: exit ${code}`);
  });
}

// A refresh waits for the events around it to settle: a reply and the thread resolution next to
// it (in either order), or the burst of replies the action-review skill posts, end in one refresh
// that sees the final state. Each new event restarts the wait, up to six waits in all. Pending
// refreshes do not survive a restart; the next event schedules one again.
const REFRESH_DELAY_MS = Number(env.REFRESH_DELAY_MS ?? 20_000);
const refreshes = new Map();
function scheduleRefresh(repo, pr) {
  const key = `${repo}#${pr}`;
  const pending = refreshes.get(key);
  if (pending) clearTimeout(pending.timer);
  const first = pending?.first ?? Date.now();
  const wait = Math.max(0, Math.min(REFRESH_DELAY_MS, first + 6 * REFRESH_DELAY_MS - Date.now()));
  const timer = setTimeout(() => {
    refreshes.delete(key);
    runResolve({ repo, pr, event: 'refresh' });
  }, wait);
  refreshes.set(key, { first, timer });
}

// Benches: a lane of their own, one bench per slot (SLOTS), each slot on its own cores. Which
// queued bench starts in which slot, and which benches a newer push or a closed PR makes moot,
// is in ci/bench-lanes.cjs. The phase lock keeps measurements apart from the other slots' work
// (BENCH_MEASURE). Each repository's bench directory holds the caches its slots share (crates,
// package stores; written by one install step at a time), passed.json and the reference store,
// which no container sees, and slot-<n>/ per slot: the PR and base checkouts with their builds
// and pinned repos, and their git directories.
const benchLanes = benchLanesLib.createLanes(SLOTS.slots);
const phases = benchLanesLib.createPhaseLock(BENCH_MEASURE);
const slotDir = (repo, id) => `${benchDir(repo)}/slot-${id}`;
let installs = Promise.resolve();
const withInstall = (fn) => (installs = installs.then(fn, fn));

async function benchStatus(repo, pr, text, { busy = false, result } = {}) {
  const { owner, name } = split(repo);
  await serialize(async () => benchComment.setBenchStatus({ github: await octokit(repo, WRITE), owner, repo: name, number: pr, status: text, busy, result }));
}
// A commit status on the PR head, so the gate shows in the checks list. Needs the App's
// "Commit statuses: write" permission; without it the comment alone reports the result.
let statusesAllowed = true;
async function commitStatus(repo, sha, state, description, url) {
  if (!statusesAllowed) return;
  const { owner, name } = split(repo);
  await serialize(async () => {
    try {
      const github = await octokit(repo, { statuses: 'write' });
      await github.rest.repos.createCommitStatus({ owner, repo: name, sha, state, context: 'Bench (review box)', description: description.slice(0, 140), target_url: url });
    } catch (error) {
      statusesAllowed = false;
      log(`commit statuses unavailable (${error.message}); the bench comment carries the result alone`);
    }
  });
}

// Code (minus the plan, site and landing page) that already passed against a base commit, per
// repository (keys from before the box served several carry no repository and match nothing).
const passedFile = (repo) => `${benchDir(repo)}/passed.json`;
const readPassed = (repo) => {
  try {
    return JSON.parse(fs.readFileSync(passedFile(repo), 'utf8'));
  } catch {
    return [];
  }
};
const recordPass = (repo, key) => fs.writeFileSync(passedFile(repo), JSON.stringify([...readPassed(repo).filter((k) => k !== key), key].slice(-500)));

// Benches made moot: queued ones are dropped, running ones stopped (their run ends as
// superseded and frees the slot). Their heads' commit statuses stop saying pending.
function settleMoot({ dropped = [], stopped = [] }, reason) {
  for (const job of stopped) {
    log(`bench ${job.repo}#${job.pr}: stopping the run for ${short(job.head)}: ${reason}`);
    job.abort?.abort();
    if (job.container) spawn('docker', ['rm', '-f', job.container], { stdio: 'ignore' });
  }
  for (const job of [...dropped, ...stopped]) commitStatus(job.repo, job.head, 'error', `Not benchmarked: ${reason}`, job.runUrl);
  if (dropped.length || stopped.length) saveQueue();
}

// What GitHub says about the PR now (read at `asOf`) makes its benches of other heads moot, or
// all of them once it is closed. Every bench request checks this first, whatever it asks for.
function supersedeBenches(repo, pull, asOf) {
  const open = pull.state === 'open';
  const moot = benchLanesLib.supersede(benchLanes, { repo, pr: pull.number }, { open, head: pull.head.sha, asOf });
  const reason = open ? `superseded by ${short(pull.head.sha)}` : 'the PR was closed';
  settleMoot(moot, reason);
  const gone = [...moot.dropped, ...moot.stopped];
  if (!open && gone.length) benchStatus(repo, pull.number, `Bench of \`${short(gone[0].head)}\` dropped: the PR was closed.`);
  if (gone.length) startBenches();
}

function enqueueBench(job) {
  job.queuedAt = Date.now();
  const { replaced, stopped } = benchLanesLib.enqueue(benchLanes, job);
  settleMoot({ dropped: replaced.filter((j) => j.head !== job.head), stopped }, `superseded by ${short(job.head)}`);
  startBenches();
  // Still queued: every slot is busy, or this PR's previous bench is still stopping.
  if (benchLanes.queue.includes(job)) {
    const ahead = benchLanesLib.ahead(benchLanes, job);
    const behind = ahead ? ` behind ${ahead} other bench${ahead === 1 ? '' : 'es'}` : '';
    benchStatus(job.repo, job.pr, `Bench of \`${short(job.head)}\` against \`${job.baseRef}\` queued${behind} (requested by @${job.by}).`, { busy: true });
    commitStatus(job.repo, job.head, 'pending', `Queued${behind} on the bench box`, job.runUrl);
  }
  saveQueue();
}

// Starts every queued bench a free slot can take; each frees its slot when it ends.
function startBenches() {
  for (const { job, slot } of benchLanesLib.assign(benchLanes)) {
    runBench(job, slot)
      .catch((error) => log(`${label(job)} crashed:`, error.stack))
      .finally(() => {
        benchLanesLib.finish(benchLanes, slot);
        saveQueue();
        startBenches();
      });
  }
  saveQueue();
}

// Below 8 GB free, the release builds of this slot and of the idle ones go (rebuilt from cached
// crates). cleanBench removes them after every run unless BENCH_KEEP_BUILDS is set; this covers
// a run that died before it could, and a box keeping its builds that runs low.
function benchDisk(repo, slot, logFile) {
  const free = () => {
    const { bavail, bsize } = fs.statfsSync(DATA);
    return (bavail * bsize) / 2 ** 30;
  };
  if (free() < 8) {
    fs.appendFileSync(logFile, `\n=== ${free().toFixed(1)} GB free: removing the release builds\n`);
    for (const other of benchLanes.slots.filter((s) => s.id === slot.id || !s.job)) {
      for (const target of ['base/target', 'pr/target/release']) removeUnder(slotDir(repo, other.id), target);
    }
  }
  return free() >= 4;
}

// Reference tools are measured once per key, in a PR's run, and reused by later runs on the same
// slot's cores while fresh. The key covers what could change them: the scenarios, pinned repos,
// bench packages, fixtures and harness at the head (prepare's ref_key), the job image (Node,
// hyperfine), the cores, the repo set and the measurement mode. Our tools are always measured,
// on both sides; only they are gated, so a reused reference row never moves the gate.
const referenceDir = (repo) => `${benchDir(repo)}/reference`;
function referenceKey(info, slot) {
  if (!info.ref_key || !IMAGE_ID || REFERENCE_DAYS <= 0) return null;
  return crypto.createHash('sha256').update([info.ref_key, IMAGE_ID, slot.cpus, BENCH_REPOS, BENCH_MEASURE].join('\n')).digest('hex').slice(0, 40);
}
function readReference(repo, key) {
  if (!key) return null;
  const file = `${referenceDir(repo)}/${key}.json`;
  try {
    if (Date.now() - fs.statSync(file).mtimeMs > REFERENCE_DAYS * 86_400_000) return null;
    return fs.readFileSync(file, 'utf8');
  } catch {
    return null;
  }
}
// The run wrote out/reference.json when it measured the reference tools. It ran PR code, so only
// names and numbers are kept, checked; the 50 newest keys stay.
const SAFE_NAME = /^[\w .:+/()-]{1,100}$/;
function storeReference(repo, key, file, sha) {
  if (!key) return;
  try {
    const parsed = JSON.parse(readOutFile(file, 4 << 20));
    if (!parsed || !Array.isArray(parsed.main) || !Array.isArray(parsed.repos)) return;
    const stat = (s) => s && ['median', 'mean', 'stddev', 'min', 'max', 'p95', 'runs'].every((k) => Number.isFinite(s[k]) && s[k] >= 0);
    const clean = (list) => (Array.isArray(list) ? list : []).filter((s) => SAFE_NAME.test(s?.id ?? '')).map((s) => ({
      id: s.id,
      tools: (Array.isArray(s.tools) ? s.tools : []).filter((t) => SAFE_NAME.test(t?.name ?? '') && t.status === 'ok' && stat(t.stats)).map((t) => ({
        name: t.name, status: 'ok', stats: Object.fromEntries(['median', 'mean', 'stddev', 'min', 'max', 'p95', 'runs'].map((k) => [k, t.stats[k]])),
        ...(Number.isFinite(t.max_rss_mb) ? { max_rss_mb: t.max_rss_mb } : {}),
      })),
    }));
    const value = { sha, at: new Date().toISOString().slice(0, 10), main: clean(parsed.main), repos: clean(parsed.repos) };
    fs.mkdirSync(referenceDir(repo), { recursive: true });
    fs.writeFileSync(`${referenceDir(repo)}/${key}.json.new`, JSON.stringify(value));
    fs.renameSync(`${referenceDir(repo)}/${key}.json.new`, `${referenceDir(repo)}/${key}.json`);
    const files = fs.readdirSync(referenceDir(repo)).filter((f) => f.endsWith('.json'))
      .map((f) => [fs.statSync(`${referenceDir(repo)}/${f}`).mtimeMs, f]).sort(([a], [b]) => b - a);
    for (const [, f] of files.slice(50)) fs.rmSync(`${referenceDir(repo)}/${f}`, { force: true });
  } catch (error) {
    log(`reference tools: nothing kept (${error.message})`);
  }
}
// The binaries the PR's scenarios run (out/bins.json, from the checkout, which runs no PR code).
function benchBins(dir) {
  try {
    const bins = JSON.parse(readOutFile(`${dir}/out/bins.json`, 65_536));
    if (Array.isArray(bins) && bins.length && bins.length < 100 && bins.every((b) => typeof b === 'string' && /^(examples\/)?[A-Za-z0-9_-]{1,64}$/.test(b))) return bins.join(',');
  } catch {}
  return '';
}

async function runBench(job, slot) {
  const { repo, pr, head, baseRef, by, runUrl } = job;
  const { owner, name } = split(repo);
  // A bench can wait a long time in the queue: one whose PR closed or moved on is not run.
  try {
    const { data: pull } = await (await octokit(repo, READ)).rest.pulls.get({ owner, repo: name, pull_number: pr });
    if (pull.state !== 'open' || pull.head.sha !== head) {
      const reason = pull.state !== 'open' ? 'the PR was closed' : `superseded by ${short(pull.head.sha)}`;
      log(`bench ${repo}#${pr} at ${short(head)} not run: ${reason}`);
      await commitStatus(repo, head, 'error', `Not benchmarked: ${reason}`, runUrl);
      if (pull.state !== 'open') await benchStatus(repo, pr, `Bench of \`${short(head)}\` dropped: the PR was closed.`);
      else await benchStatus(repo, pr, `Bench of \`${short(head)}\` not run: the PR moved on to \`${short(pull.head.sha)}\`; push again or comment /bench to bench it.`);
      return;
    }
  } catch (error) {
    log(`bench ${repo}#${pr}: could not check the PR (${error.message}); running it`);
  }
  if (job.cancelled) return;
  job.abort = new AbortController();
  const id = jobId(repo, pr);
  const dir = `${DATA}/jobs/${id}-bench`;
  const logFile = `${DATA}/logs/${id}-bench.log`;
  for (const sub of ['in', 'out']) fs.mkdirSync(`${dir}/${sub}`, { recursive: true });
  const BENCH = benchDir(repo);
  const SLOT = slotDir(repo, slot.id);
  for (const sub of ['cargo', 'pm', 'npm']) fs.mkdirSync(`${BENCH}/${sub}`, { recursive: true });
  for (const sub of ['pr', 'base', 'git']) fs.mkdirSync(`${SLOT}/${sub}`, { recursive: true });
  const base = { REPO: repo, PR_NUMBER: String(pr), JOB_DIR: dir, CACHE_DIR: cacheDir(repo), BENCH_DIR: SLOT, BENCH_REPOS };
  const started = Date.now();
  const since = utc();
  const title = `Bench of \`${short(head)}\` against \`${baseRef}\``;
  const where = SLOTS.slots.length > 1 ? ` on cores ${slot.cpus}` : '';
  const progress = async (phase) => job.cancelled || benchStatus(repo, pr, `${title} running since ${since}${where} (requested by @${by}): ${phase}.`, { busy: true });
  log(`bench ${repo}#${pr} at ${short(head)} for @${by} in slot ${slot.id} (${slot.cpus}): ${logFile}`);
  await progress('fetching');
  commitStatus(repo, head, 'pending', `Running since ${since} on the bench box`, runUrl);
  const step = (stepName, args, envs, timeoutMs) => {
    if (job.cancelled) return 'cancelled';
    job.container = `tsr-${stepName}-${id}`;
    return docker(args, { name: job.container, envs: { ...base, ...envs }, timeoutMs, logFile });
  };
  // Installing and building share the box; measuring waits for a quiet one (BENCH_MEASURE).
  // A bench stopped while it waits for its turn gives the turn and its slot up at once.
  const stoppable = { signal: job.abort.signal, aborted: 'cancelled' };
  const work = (fn) => phases.run('work', fn, () => progress('waiting for another slot to finish measuring'), stoppable);
  const measure = (fn) => phases.run('measure', fn, () => progress('waiting for the other slots to finish building'), stoppable);
  // Every step that runs code from the PR gets no token, the git directories and the repo cache
  // read-only, and only the checkout it needs writable: the base's build sees no PR code, and no
  // PR step can write the base's build. passed.json and the reference store are never mounted.
  const cpus = ['--cpuset-cpus', slot.cpus];
  const checkout = (side, mode = 'rw') => ['-v', `${SLOT}/${side}:${SLOT}/${side}${mode === 'ro' ? ':ro' : ''}`];
  // in/ and out/ are mounted on their own, so no step can swap them for a link elsewhere; the
  // dispatcher reads what they write with readOutFile.
  const mounts = ['-v', `${SLOT}/git:${SLOT}/git:ro`, '-v', `${cacheDir(repo)}:${cacheDir(repo)}:ro`, '-v', `${dir}/in:${dir}/in:ro`, '-v', `${dir}/out:${dir}/out`,
    ...['cargo', 'pm', 'npm'].flatMap((sub) => ['-v', `${BENCH}/${sub}:${BENCH}/${sub}`]),
    '-e', `CARGO_HOME=${BENCH}/cargo`, '-e', `TS_R_BENCH_CACHE=${BENCH}/pm`, '-e', `npm_config_cache=${BENCH}/npm`,
    '-e', `CARGO_BUILD_JOBS=${Math.min(4, slot.size)}`, '-e', `RAYON_NUM_THREADS=${Math.min(4, slot.size)}`];
  const heavy = [...LOCKED_DOWN, ...cpus, '--memory', BENCH_MEMORY, '--pids-limit', '4096'];
  const offline = ['--network', 'none', '-e', 'CARGO_NET_OFFLINE=true', '-e', 'npm_config_offline=true'];

  let info = null;
  let code = 2;
  const prepared = await withCache(async () => step('bench-prepare', ['--network', 'bridge', ...LOCKED_DOWN, ...cpus, '-v', `${dir}:${dir}`, '-v', `${cacheDir(repo)}:${cacheDir(repo)}`, IMAGE, 'node', '/app/job.mjs', 'bench-prepare'],
    { GH_TOKEN: await token(repo, READ), HEAD_SHA: head, BASE_REF: baseRef }, 10 * 60_000));
  if (prepared === 0) info = JSON.parse(fs.readFileSync(`${dir}/in/bench.json`, 'utf8'));
  const passKey = info && `${repo}:${info.base_key}-${info.key}`;
  let outcome;
  if (job.cancelled || prepared === 3) {
    outcome = 'superseded';
  } else if (info && readPassed(repo).includes(passKey)) {
    outcome = 'already passed';
    await benchStatus(repo, pr, `\`${short(head)}\` has the same code as a commit already benchmarked against the same \`${baseRef}\` code, which passed; not run again.`);
    await commitStatus(repo, head, 'success', 'Same code already passed against this base', runUrl);
  } else if (info && !benchDisk(repo, slot, logFile)) {
    outcome = 'no disk';
    await benchStatus(repo, pr, `${title} could not run: the bench box is out of disk.`);
    await commitStatus(repo, head, 'error', 'The bench box is out of disk', runUrl);
  } else if (info) {
    const refKey = referenceKey(info, slot);
    const reference = readReference(repo, refKey);
    if (reference) fs.writeFileSync(`${dir}/in/reference.json`, reference);
    const checkedOut = await work(() => step('bench-checkout', ['--network', 'none', ...LOCKED_DOWN, ...cpus, '-v', `${SLOT}:${SLOT}`, '-v', `${cacheDir(repo)}:${cacheDir(repo)}:ro`,
      '-v', `${dir}/in:${dir}/in:ro`, '-v', `${dir}/out:${dir}/out`, IMAGE, 'node', '/app/job.mjs', 'bench-checkout'], {}, 10 * 60_000));
    // Only what the PR's scenarios run is built (all of the workspace if that is unknown).
    const bins = checkedOut === 0 ? benchBins(dir) : '';
    if (checkedOut === 0) await progress('installing the pinned repos and tools');
    // The caches are shared by the slots: one install at a time writes them.
    const installed = checkedOut === 0 && await withInstall(() => work(() => step('bench-deps', ['--network', 'ts-r-review-egress-deps', ...heavy, ...checkout('pr'), ...checkout('base', 'ro'), ...mounts,
      ...['HTTPS_PROXY', 'HTTP_PROXY', 'https_proxy', 'http_proxy'].flatMap((proxy) => ['-e', `${proxy}=http://egress-deps:3128`]),
      // Corepack downloads pnpm and Yarn with Node's own fetch, which uses the proxy only with this.
      '-e', 'NODE_USE_ENV_PROXY=1', '-e', 'COREPACK_ENABLE_DOWNLOAD_PROMPT=0',
      IMAGE, 'node', '/app/job.mjs', 'bench-deps'], {}, 40 * 60_000)));
    let failed = checkedOut !== 0 ? 'the checkout' : installed !== 0 ? 'installing the pinned repos and tools' : null;
    for (const side of ['base', 'pr']) {
      if (failed) break;
      await progress(`building the ${side === 'pr' ? 'PR' : 'base'}`);
      const built = await work(() => step(`bench-build-${side}`, [...offline, ...heavy, ...checkout(side), ...mounts, IMAGE, 'node', '/app/job.mjs', 'bench-build'],
        { BENCH_SIDE: side, BENCH_BINS: bins }, 75 * 60_000));
      if (built !== 0) failed = `building the ${side === 'pr' ? 'PR' : 'base'}`;
    }
    if (!failed) {
      // The run writes its phase to out/phase; the comment follows it.
      let shown = '';
      const watch = setInterval(() => {
        let phase = '';
        try {
          phase = (readOutFile(`${dir}/out/phase`, 4096) ?? '').trim().slice(0, 200);
        } catch {}
        if (phase && phase !== shown) {
          shown = phase;
          progress(phase);
        }
      }, 15_000);
      await measure(() => step('bench-measure', [...offline, ...heavy, ...checkout('pr'), ...checkout('base', 'ro'), ...mounts, IMAGE, 'node', '/app/job.mjs', 'bench-measure'],
        {}, 120 * 60_000));
      clearInterval(watch);
      try {
        code = JSON.parse(readOutFile(`${dir}/out/bench.json`, 4096)).code;
      } catch {}
      // A fork's run does not fill the store that later PRs read.
      if (!reference && !job.cancelled && !job.fork && (code === 0 || code === 1)) storeReference(repo, refKey, `${dir}/out/reference.json`, head);
    }
    if (job.cancelled) {
      outcome = 'superseded';
    } else {
      outcome = ['passed', 'regression'][code] ?? 'error';
      const tables = readOutFile(`${dir}/out/comment.md`, 1 << 20) ?? '';
      const duration = `${Math.max(1, Math.round((Date.now() - started) / 60_000))} min`;
      const done = !failed ? `Benchmarked \`${short(head)}\` against \`${baseRef}\` @ \`${short(info.base)}\` in ${duration}`
        : `${title} did not run: ${failed} failed after ${duration}`;
      await benchStatus(repo, pr, `${done} (requested by @${by}).`, {
        result: { sha: head, base: info.base, baseRef, duration, code, log: `logs/${id}-bench.log`, tables },
      });
      await commitStatus(repo, head, ['success', 'failure'][code] ?? 'error', ['No regression', 'A tool is slower than the base beyond the gate'][code] ?? 'The bench did not complete', runUrl);
      if (code === 0) recordPass(repo, passKey);
    }
  } else {
    outcome = 'prepare failed';
    await benchStatus(repo, pr, `${title} could not start: fetching the PR failed. Push again or re-run the Bench workflow to retry.`);
    await commitStatus(repo, head, 'error', 'Fetching the PR failed', runUrl);
  }
  fs.rmSync(dir, { recursive: true, force: true });
  cleanBench(SLOT);
  pruneLogs();
  log(`bench ${repo}#${pr} done in slot ${slot.id}: ${outcome}`);
}

// Requests. The body is only a pointer; what to do and for whom comes from GitHub, and who may
// start what is decided in ci/trust.cjs.
const seenRuns = new Set();
// A run id is used once. It is reserved while its request is handled, and released if handling
// throws (a GitHub API hiccup), so the workflow's retry can still get through.
async function handle(body) {
  const request = trust.parseRequest(body, REPOS);
  if (request.error) return request.error;
  const key = `${request.repo}#${request.runId}`;
  if (seenRuns.has(key)) return [409, 'repeated run'];
  seenRuns.add(key);
  try {
    return await dispatch(request);
  } catch (error) {
    seenRuns.delete(key);
    throw error;
  }
}

const eyes = (github, repo, comment) => github.rest.reactions.createForIssueComment({ owner: split(repo).owner, repo: split(repo).name, comment_id: comment.id, content: 'eyes' }).catch(() => {});

async function dispatch({ runId, pr, repo, commentId }) {
  const { owner, name } = split(repo);
  const github = await octokit(repo, READ);
  const { data: run } = await github.rest.actions.getWorkflowRun({ owner, repo: name, run_id: runId });
  const stale = trust.checkRun(run, { repo, workflows: [WORKFLOW, BENCH_WORKFLOW], now: Date.now() });
  if (stale) return stale;
  const actor = run.triggering_actor?.login ?? run.actor?.login;
  // Benches queued before this moment are judged by the PR as read now (supersedeBenches).
  const asOf = Date.now();
  const { data: pull } = await github.rest.pulls.get({ owner, repo: name, pull_number: pr });
  const getComment = async () => {
    if (!commentId) return null;
    return (await github.rest.issues.getComment({ owner, repo: name, comment_id: commentId })).data;
  };
  if (!LANES.has(run.path === BENCH_WORKFLOW ? 'bench' : 'review')) return [421, `this box does not run ${run.path === BENCH_WORKFLOW ? 'benches' : 'reviews'}`];
  if (run.path === BENCH_WORKFLOW) return dispatchBench({ github, run, actor, pull, repo, getComment, asOf });

  const comment = run.event === 'issue_comment' ? await getComment() : null;
  const decision = trust.reviewDecision({ run, actor, pull, comment, repo, trusted: TRUSTED });
  if (decision.reject) return decision.reject;
  const { action, by, react, canFix } = decision;
  if (react) await eyes(github, repo, comment);
  const reviewJob = { repo, pr, head: pull.head.sha, by, runUrl: run.html_url };
  switch (action) {
    case 'auto-review':
      // One automatic review per PR (reopening it or marking it ready again does not start
      // another); after that a review takes an @autoreview comment.
      if (await review.findSummary(github, owner, name, pr)) return [409, 'already reviewed; comment @autoreview for another review'];
      enqueueReview(reviewJob);
      return [202, 'review queued'];
    case 'review':
      enqueueReview(reviewJob);
      return [202, 'review queued'];
    case 'fix':
      enqueue({ ...reviewJob, kind: 'fix' });
      return [202, 'fix queued'];
    case 'approve':
      runResolve({ repo, pr, event: 'approve', action: 'created', commentId: comment.id, actor: by });
      return [202, 'approve queued'];
    case 'resolve':
      runResolve({ repo, pr, event: 'issue_comment', action: 'created', commentId: comment.id, actor: by });
      return [202, 'resolve queued'];
    case 'refresh':
      scheduleRefresh(repo, pr);
      return [202, 'refresh queued'];
    case 'summary-edit': {
      // Ticking the box asks Codex to fix the open findings; unticking (or a fork) is a plain
      // bulk resolve or reopen.
      const threads = await review.listThreads(github, owner, name, pr);
      if (canFix && review.bulkIntent({ action: 'edited', comment }, threads) === true) {
        enqueue({ ...reviewJob, kind: 'fix' });
        return [202, 'fix queued'];
      }
      runResolve({ repo, pr, event: 'issue_comment', action: 'edited', commentId: comment.id, actor: by });
      return [202, 'checkbox queued'];
    }
    default:
      return [500, `unhandled decision ${action}`];
  }
}

// A bench builds and runs the PR's code (offline, with no token). It runs by itself only for a
// trusted author's PR from the same repository (the top PR of a stack, as on CI); a trusted
// user's approval of the head, a trusted user's /bench comment or a manual run starts one for any
// PR, forks included (ci/trust.cjs). Before any of that, any accepted bench run (a push, the PR
// closing, a review or comment on it) drops the PR's benches that GitHub says are moot: those of
// another head, or all of them once the PR is closed. That only stops work, never starts it.
async function dispatchBench({ github, run, actor, pull, repo, getComment, asOf }) {
  const { owner, name } = split(repo);
  supersedeBenches(repo, pull, asOf);
  const needs = trust.benchNeeds(run);
  const facts = {};
  if (needs.above && pull.state === 'open') {
    facts.above = (await github.rest.pulls.list({ owner, repo: name, base: pull.head.ref, state: 'open', per_page: 100 })).data
      // Only PRs from this repository stack on a branch of it.
      .filter((p) => p.head.repo?.full_name === repo);
  }
  if (needs.reviews && pull.state === 'open') {
    facts.reviews = await github.paginate(github.rest.pulls.listReviews, { owner, repo: name, pull_number: pull.number, per_page: 100 });
  }
  if (needs.comment) facts.comment = await getComment();
  const decision = trust.benchDecision({ run, actor, pull, repo, trusted: TRUSTED, ...facts });
  if (decision.reject) return decision.reject;
  if (decision.react) await eyes(github, repo, facts.comment);
  const job = { kind: 'bench', repo, pr: pull.number, head: pull.head.sha, baseRef: pull.base.ref, by: decision.by, runUrl: run.html_url, fork: pull.head.repo?.full_name !== repo };
  if (benchLanes.slots.some(({ job: j }) => j && samePr(j, job) && j.head === job.head && !j.cancelled)) return [409, 'this head is being benchmarked now'];
  enqueueBench(job);
  return [202, 'bench queued'];
}

const server = http.createServer((req, res) => {
  const reply = (code, text) => res.writeHead(code, { 'content-type': 'text/plain' }).end(`${text}\n`);
  const url = req.url.replace(/^\/code-review/, '');
  const lane = (now, waiting) => `running ${now ? label(now) : 'nothing'}; queued ${waiting.map(label).join(' ') || 'nothing'}`;
  const slots = benchLanes.slots.map((s) => `[${s.cpus}] ${s.job ? `${label(s.job)}${s.job.cancelled ? ' (stopping)' : ''}` : 'idle'}`).join(', ');
  const benches = `slots ${slots}; queued ${benchLanes.queue.map(label).join(' ') || 'nothing'}; measure ${BENCH_MEASURE}${SLOTS.error ? `; BENCH_SLOTS ignored: ${SLOTS.error}` : ''}`;
  if (req.method === 'GET' && url === '/healthz') return reply(200, `ok; repos: ${REPOS.join(', ')}; codex: ${lane(running, queue)}; bench: ${benches}`);
  if (req.method !== 'POST' || url !== '/jobs') return reply(404, 'not found');
  const auth = Buffer.from(req.headers.authorization ?? '');
  const expected = Buffer.from(`Bearer ${DISPATCH_TOKEN}`);
  if (auth.length !== expected.length || !crypto.timingSafeEqual(auth, expected)) return reply(401, 'unauthorized');
  let raw = '';
  req.on('data', (chunk) => {
    raw += chunk;
    if (raw.length > 16_384) req.destroy();
  });
  req.on('end', async () => {
    try {
      let body;
      try {
        body = JSON.parse(raw);
      } catch {
        reply(400, 'malformed request');
        return;
      }
      const [code, text] = await handle(body);
      log(`request ${raw} -> ${code} ${text}`);
      reply(code, text);
    } catch (error) {
      log(`request ${raw} failed:`, error.message);
      reply(500, 'error');
    }
  });
});

for (const dir of ['jobs', 'logs', 'cache', 'codex-home', 'targets', 'bench']) fs.mkdirSync(`${DATA}/${dir}`, { recursive: true });
// Leftovers from a restart: job directories, build directories, bench builds, and containers
// that outlived the dispatcher.
for (const parent of ['jobs', 'targets']) {
  for (const dir of fs.readdirSync(`${DATA}/${parent}`)) fs.rmSync(path.join(DATA, parent, dir), { recursive: true, force: true });
}
// The cache of the box's only repository, from before it served several, becomes the first one's.
if (fs.existsSync(`${DATA}/cache/repo.git`) && !fs.existsSync(cacheDir(REPOS[0]))) fs.renameSync(`${DATA}/cache/repo.git`, cacheDir(REPOS[0]));
// So do its bench directories, from before each repo had its own.
if (!fs.existsSync(benchDir(REPOS[0]))) {
  const old = fs.readdirSync(`${DATA}/bench`).filter((d) => !d.includes('__'));
  fs.mkdirSync(benchDir(REPOS[0]), { recursive: true });
  for (const d of old) fs.renameSync(`${DATA}/bench/${d}`, `${benchDir(REPOS[0])}/${d}`);
}
// Each job mounts its own repo's cache and bench directory, created here so Docker does not
// create them as root.
for (const repo of REPOS) for (const dir of [cacheDir(repo), benchDir(repo)]) fs.mkdirSync(dir, { recursive: true });
for (const repo of REPOS) {
  const bench = benchDir(repo);
  // The checkouts of the single lane of before become slot 0's, builds included.
  if (fs.existsSync(`${bench}/git`) && !fs.existsSync(slotDir(repo, 0))) {
    fs.mkdirSync(slotDir(repo, 0));
    for (const sub of ['pr', 'base', 'git']) if (fs.existsSync(`${bench}/${sub}`)) fs.renameSync(`${bench}/${sub}`, `${slotDir(repo, 0)}/${sub}`);
  }
  // Slots beyond BENCH_SLOTS (it was lowered) go, with their checkouts and builds.
  for (const dir of fs.readdirSync(bench)) {
    const n = /^slot-(\d+)$/.exec(dir);
    if (n && Number(n[1]) >= benchLanes.slots.length) fs.rmSync(`${bench}/${dir}`, { recursive: true, force: true });
  }
  for (const slot of benchLanes.slots) if (fs.existsSync(slotDir(repo, slot.id))) cleanBench(slotDir(repo, slot.id));
}
log(`bench slots: ${benchLanes.slots.map((s) => s.cpus).join(' ')} (${HOST_CORES ?? 'unknown'} cores; measure ${BENCH_MEASURE})${SLOTS.error ? `; BENCH_SLOTS ignored: ${SLOTS.error}` : ''}`);
// Synchronous, so it cannot remove the containers of the jobs restoreQueue starts.
spawnSync('sh', ['-c', 'docker ps -aq --filter name=^tsr- | xargs -r docker rm -f'], { stdio: 'ignore' });
server.listen(Number(env.PORT ?? 8787), '0.0.0.0', () => log(`dispatcher for ${REPOS.join(', ')} on :${env.PORT ?? 8787}; trusted: ${[...TRUSTED].join(', ')}`));
restoreQueue();
