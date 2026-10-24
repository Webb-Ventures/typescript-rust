// The bench lane's scheduling, with no I/O so it can be tested (bench-lanes.test.cjs): which CPU
// cores each bench slot gets, which queued bench starts in which slot, which benches a newer
// push or a closed PR makes moot, and the lock that keeps measurements apart from other work.
// The dispatcher (box/dispatcher.mjs) runs the containers.
'use strict';

const MAX_CPU = 4096;

// A Docker cpuset ("2-5", "2,3,8-11") as a sorted list of core numbers, or null if malformed.
function parseCpus(text) {
  if (typeof text !== 'string' || !/^\d+(-\d+)?(,\d+(-\d+)?)*$/.test(text.trim())) return null;
  const cores = new Set();
  for (const part of text.trim().split(',')) {
    const [from, to = from] = part.split('-').map(Number);
    if (from > to || to >= MAX_CPU) return null;
    for (let c = from; c <= to; c++) cores.add(c);
  }
  return [...cores].sort((a, b) => a - b);
}

// The shortest cpuset for a list of cores: [2, 3, 4, 5, 8] -> "2-5,8".
function formatCpus(cores) {
  const parts = [];
  for (let i = 0; i < cores.length; i++) {
    let j = i;
    while (j + 1 < cores.length && cores[j + 1] === cores[j] + 1) j++;
    parts.push(j === i ? `${cores[i]}` : `${cores[i]}-${cores[j]}`);
    i = j;
  }
  return parts.join(',');
}

// The bench slots, each a cpuset of its own, from config.env:
//   BENCH_SLOTS unset          one slot on BENCH_CPUS (default 2-5): the single lane of before.
//   BENCH_SLOTS=auto           as many slots of BENCH_SLOT_CPUS cores (default 4) as fit in the
//                              cores Docker reports, after REVIEW_CPUS when this box runs reviews.
//   BENCH_SLOTS=<n>            n such slots, from the lowest free cores up.
//   BENCH_SLOTS=["2-5","6-9"]  these cpusets, which must not overlap each other or REVIEW_CPUS.
// `cores` is the host's core count, or null when unknown (then only explicit cpusets work).
// A bad setting falls back to the single default slot and says why in `error`, so a typo slows
// benches down rather than letting two of them share cores.
function benchSlots(config, cores) {
  const fallback = config.cpus ?? '2-5';
  const single = (error) => ({ slots: [parseCpus(fallback) ? fallback : '2-5'], error });
  const setting = config.slots?.trim();
  if (!setting) return parseCpus(fallback) ? { slots: [fallback] } : single(`BENCH_CPUS=${fallback} is not a cpuset`);
  const known = Number.isInteger(cores) && cores > 0;
  const reserved = config.review ? parseCpus(config.reviewCpus ?? '0-1') : [];
  if (!reserved) return single(`REVIEW_CPUS=${config.reviewCpus} is not a cpuset`);
  let sets;
  if (setting.startsWith('[')) {
    let list;
    try {
      list = JSON.parse(setting);
    } catch {
      return single('BENCH_SLOTS is not a JSON list of cpusets');
    }
    if (!Array.isArray(list) || !list.length || !list.every((s) => typeof s === 'string' && parseCpus(s))) {
      return single('BENCH_SLOTS must list cpusets like "2-5"');
    }
    sets = list.map((s) => parseCpus(s));
  } else {
    const size = Number(config.slotCpus ?? 4);
    if (!Number.isInteger(size) || size < 1) return single(`BENCH_SLOT_CPUS=${config.slotCpus} is not a core count`);
    if (!known) return single(`BENCH_SLOTS=${setting} needs the box's core count, which Docker did not report; list the cpusets instead`);
    const free = [...Array(cores).keys()].filter((c) => !reserved.includes(c));
    const fit = Math.floor(free.length / size);
    const count = setting === 'auto' ? fit : Number(setting);
    if (!Number.isInteger(count) || count < 1) {
      return single(setting === 'auto' ? `${free.length} free cores hold no slot of ${size}` : `BENCH_SLOTS=${setting} is not auto, a count or a list`);
    }
    if (count > fit) return single(`${count} slots of ${size} cores need ${count * size} free cores; the box has ${free.length}`);
    sets = [...Array(count).keys()].map((i) => free.slice(i * size, (i + 1) * size));
  }
  const seen = new Set(reserved);
  for (const set of sets) {
    for (const c of set) {
      if (known && c >= cores) return single(`core ${c} is not on this ${cores}-core box`);
      if (seen.has(c)) return single(`core ${c} is in two slots${reserved.includes(c) ? ' (or in REVIEW_CPUS)' : ''}`);
      seen.add(c);
    }
  }
  return { slots: sets.map(formatCpus) };
}

const samePr = (a, b) => a.repo === b.repo && a.pr === b.pr;

// The queue and the slots. A slot remembers the PR and base it last ran, so the next bench of
// either goes back to the checkout that already has its build.
function createLanes(slots) {
  return { queue: [], slots: slots.map((cpus, id) => ({ id, cpus, size: parseCpus(cpus)?.length ?? 1, job: null, last: null })) };
}

const running = (lanes) => lanes.slots.map((s) => s.job).filter(Boolean);

// Benches of a PR that GitHub's current state of it makes moot: another head, or the PR closed.
// Only work queued before `asOf` (when that state was read) counts, so a request that read the
// PR a moment before a newer push was queued cannot drop the newer bench. Queued ones leave the
// queue; running ones are marked cancelled and returned, for the caller to stop.
function supersede(lanes, { repo, pr }, { open, head, asOf = Infinity }) {
  const moot = (job) => job.repo === repo && job.pr === pr && (job.queuedAt ?? 0) <= asOf && (!open || job.head !== head);
  const dropped = lanes.queue.filter(moot);
  lanes.queue = lanes.queue.filter((job) => !moot(job));
  const stopped = running(lanes).filter((job) => !job.cancelled && moot(job));
  for (const job of stopped) job.cancelled = open ? `superseded by ${head.slice(0, 7)}` : 'the PR was closed';
  return { dropped, stopped };
}

// Queues a bench. It replaces the PR's queued one, and a running one for another head stops.
function enqueue(lanes, job) {
  const replaced = lanes.queue.filter((j) => samePr(j, job));
  lanes.queue = lanes.queue.filter((j) => !samePr(j, job));
  const { stopped } = supersede(lanes, job, { open: true, head: job.head });
  lanes.queue.push(job);
  return { replaced, stopped };
}

// How many benches go before `job`: the running ones (not stopping) and those queued ahead.
function ahead(lanes, job) {
  const index = lanes.queue.indexOf(job);
  return running(lanes).filter((j) => !j.cancelled).length + (index < 0 ? 0 : index);
}

// Starts what can start, in queue order: each bench takes a free slot, preferring the one that
// last ran its PR (its checkout holds that build), then one that last ran its base. A PR with a
// bench still in a slot (one stopping counts: it is still cleaning up) waits, so one PR never
// runs twice at once.
function assign(lanes) {
  const started = [];
  for (let i = 0; i < lanes.queue.length;) {
    const job = lanes.queue[i];
    const free = lanes.slots.filter((s) => !s.job);
    if (!free.length) break;
    if (running(lanes).some((j) => samePr(j, job))) {
      i++;
      continue;
    }
    const slot = free.find((s) => s.last && samePr(s.last, job))
      ?? free.find((s) => s.last && s.last.repo === job.repo && s.last.baseRef === job.baseRef)
      ?? free[0];
    lanes.queue.splice(i, 1);
    slot.job = job;
    slot.last = { repo: job.repo, pr: job.pr, baseRef: job.baseRef };
    started.push({ job, slot });
  }
  return started;
}

function finish(lanes, slot) {
  lanes.slots[slot.id].job = null;
}

// Keeps measurements apart from the rest of the bench work. 'work' (installing, building) is
// shared: slots do it side by side. 'measure' is exclusive: in mode 'alone' it waits until no
// other slot measures or works, and holds both off until it ends, so a measurement sees a box as
// quiet as with one slot; in mode 'parallel' slots only keep to their own cores. Waiters are
// served in order, so a measurement is never starved by a stream of builds.
function createPhaseLock(mode = 'alone') {
  const holders = { work: 0, measure: 0 };
  const waiting = [];
  const fits = (kind) => mode !== 'alone' || (kind === 'work' ? holders.measure === 0 : holders.measure === 0 && holders.work === 0);
  const pump = () => {
    while (waiting.length && fits(waiting[0].kind)) {
      const { kind, grant } = waiting.shift();
      holders[kind]++;
      grant();
    }
  };
  // A promise of the release function (or of null, if `signal` aborted the wait), and whether it
  // had to wait.
  const acquire = (kind, signal) => {
    let entry;
    const granted = new Promise((resolve) => {
      let released = false;
      entry = {
        kind,
        grant: () => resolve(() => {
          if (released) return;
          released = true;
          holders[kind]--;
          pump();
        }),
        drop: () => resolve(null),
      };
    });
    if (signal?.aborted) {
      entry.drop();
      return { granted, waited: false };
    }
    waiting.push(entry);
    pump();
    const waited = waiting.includes(entry);
    // A bench stopped while it waits leaves the line at once, and frees its slot.
    if (waited && signal) {
      signal.addEventListener('abort', () => {
        const at = waiting.indexOf(entry);
        if (at < 0) return;
        waiting.splice(at, 1);
        entry.drop();
        pump();
      }, { once: true });
    }
    return { granted, waited };
  };
  // Runs `fn` holding the lock; `onWait` is called first if it has to wait. Returns `aborted`
  // without running `fn` if `signal` aborts first.
  async function run(kind, fn, onWait, { signal, aborted } = {}) {
    const { granted, waited } = acquire(kind, signal);
    if (waited && onWait) onWait();
    const release = await granted;
    if (!release) return aborted;
    try {
      return await fn();
    } finally {
      release();
    }
  }
  return { run, acquire, state: () => ({ ...holders, waiting: waiting.length }) };
}

module.exports = { parseCpus, formatCpus, benchSlots, createLanes, supersede, enqueue, ahead, assign, finish, createPhaseLock, samePr };
