// Tests for the bench lane's scheduling: node --test .agents/plugins/code-review/ci/*.test.cjs
'use strict';

const assert = require('node:assert/strict');
const { test } = require('node:test');
const lanes = require('./bench-lanes.cjs');

const job = (pr, head, extra = {}) => ({ kind: 'bench', repo: 'o/r', pr, head, baseRef: 'main', queuedAt: 0, ...extra });

test('cpusets parse and print', () => {
  assert.deepEqual(lanes.parseCpus('2-5'), [2, 3, 4, 5]);
  assert.deepEqual(lanes.parseCpus('8,2-3'), [2, 3, 8]);
  for (const bad of ['', '5-2', 'a', '1-', '2,,3', '99999', undefined]) assert.equal(lanes.parseCpus(bad), null, String(bad));
  assert.equal(lanes.formatCpus([2, 3, 4, 5, 8, 10, 11]), '2-5,8,10-11');
});

test('memory sizes parse as Docker writes them', () => {
  assert.equal(lanes.memoryMb('6g'), 6144);
  assert.equal(lanes.memoryMb('6G'), 6144);
  assert.equal(lanes.memoryMb('2560m'), 2560);
  assert.equal(lanes.memoryMb('2097152k'), 2048);
  assert.equal(lanes.memoryMb(String(3 * 2 ** 30)), 3072);
  for (const bad of ['', '6gb', '-1g', '0g', 'x', '1.5g', undefined, null]) assert.equal(lanes.memoryMb(bad), null, String(bad));
});

test('a bench build runs as many compiles as the slot has memory for', () => {
  // The dispatcher's defaults (6g, 4608m) on the 6-core review box's one 4-core slot: one at a
  // time, as ts_r_lint's library alone peaks at 4.3 GB; a 16g container builds three.
  assert.equal(lanes.buildJobs({ cores: 4, memory: '6g', jobMemory: '4608m' }), 1);
  assert.equal(lanes.buildJobs({ cores: 4, memory: '16g', jobMemory: '4608m' }), 3);
  assert.equal(lanes.buildJobs({ cores: 4, memory: '6g', jobMemory: '3g' }), 2);
  assert.equal(lanes.buildJobs({ cores: 4, memory: '6g', jobMemory: '4g' }), 1);
  // Memory to spare: the cores decide, at most 4.
  assert.equal(lanes.buildJobs({ cores: 4, memory: '16g', jobMemory: '3g' }), 4);
  assert.equal(lanes.buildJobs({ cores: 8, memory: '64g', jobMemory: '3g' }), 4);
  assert.equal(lanes.buildJobs({ cores: 2, memory: '64g', jobMemory: '3g' }), 2);
  // Never below one, even when a single compile needs more than the container has.
  assert.equal(lanes.buildJobs({ cores: 4, memory: '2g', jobMemory: '3g' }), 1);
  // A malformed size leaves the cores to decide, as before.
  assert.equal(lanes.buildJobs({ cores: 4, memory: 'x', jobMemory: '3g' }), 4);
  assert.equal(lanes.buildJobs({ cores: 4, memory: '6g', jobMemory: 'x' }), 4);
});

test('without BENCH_SLOTS there is one slot on BENCH_CPUS, as before', () => {
  assert.deepEqual(lanes.benchSlots({}, null), { slots: ['2-5'] });
  assert.deepEqual(lanes.benchSlots({ cpus: '4-7' }, 16), { slots: ['4-7'] });
  assert.equal(lanes.benchSlots({ cpus: 'x' }, 16).slots[0], '2-5');
});

test('auto sizes slots from the cores the box reports, after the review cores', () => {
  assert.deepEqual(lanes.benchSlots({ slots: 'auto', review: true }, 6), { slots: ['2-5'] });
  assert.deepEqual(lanes.benchSlots({ slots: 'auto', review: true }, 16), { slots: ['2-5', '6-9', '10-13'] });
  assert.deepEqual(lanes.benchSlots({ slots: 'auto', review: false }, 16), { slots: ['0-3', '4-7', '8-11', '12-15'] });
  assert.deepEqual(lanes.benchSlots({ slots: 'auto', review: true, reviewCpus: '0-3', slotCpus: '6' }, 16), { slots: ['4-9', '10-15'] });
  assert.deepEqual(lanes.benchSlots({ slots: '2', review: true }, 16), { slots: ['2-5', '6-9'] });
});

test('slots never share a core, and a bad setting falls back to one slot', () => {
  assert.deepEqual(lanes.benchSlots({ slots: '["2-5","6-9"]', review: true }, 16), { slots: ['2-5', '6-9'] });
  for (const [config, cores, why] of [
    [{ slots: '["2-5","5-8"]' }, 16, /core 5 is in two slots/],
    [{ slots: '["0-3"]', review: true }, 16, /REVIEW_CPUS/],
    [{ slots: '["2-5","14-17"]' }, 16, /core 16 is not on this 16-core box/],
    [{ slots: 'auto', review: true }, null, /core count/],
    [{ slots: '4', review: true }, 6, /need 16 free cores/],
    [{ slots: 'auto', review: true }, 4, /hold no slot/],
    [{ slots: '[2]' }, 16, /cpusets/],
    [{ slots: '[' }, 16, /JSON/],
    [{ slots: 'many' }, 16, /not auto/],
  ]) {
    const result = lanes.benchSlots(config, cores);
    assert.deepEqual(result.slots, ['2-5'], JSON.stringify(config));
    assert.match(result.error, why);
  }
  // Without the core count, listed cpusets still work (they are checked against each other).
  assert.deepEqual(lanes.benchSlots({ slots: '["2-5","6-9"]' }, null), { slots: ['2-5', '6-9'] });
});

test('several slots run benches of different PRs at once, each on its own cores', () => {
  const l = lanes.createLanes(['2-5', '6-9']);
  for (const j of [job(1, 'a'), job(2, 'b'), job(3, 'c')]) lanes.enqueue(l, j);
  const started = lanes.assign(l);
  assert.deepEqual(started.map(({ job: j, slot }) => [j.pr, slot.cpus, slot.size]), [[1, '2-5', 4], [2, '6-9', 4]]);
  assert.deepEqual(l.queue.map((j) => j.pr), [3]);
  assert.equal(lanes.ahead(l, l.queue[0]), 2);
  lanes.finish(l, started[1].slot);
  assert.deepEqual(lanes.assign(l).map(({ job: j, slot }) => [j.pr, slot.cpus]), [[3, '6-9']]);
});

test('a slot goes back to the PR it ran last, then to its base', () => {
  const l = lanes.createLanes(['2-5', '6-9', '10-13']);
  lanes.enqueue(l, job(1, 'a'));
  lanes.enqueue(l, job(2, 'b', { baseRef: 'm1/front-end' }));
  const first = lanes.assign(l);
  for (const { slot } of first) lanes.finish(l, slot);
  lanes.enqueue(l, job(3, 'c', { baseRef: 'm1/front-end' }));
  lanes.enqueue(l, job(1, 'a2'));
  const second = lanes.assign(l);
  assert.deepEqual(second.map(({ job: j, slot }) => [j.pr, slot.id]), [[3, 1], [1, 0]]);
});

test('a newer push replaces the queued bench and stops the running one', () => {
  const l = lanes.createLanes(['2-5']);
  const old = job(1, 'a');
  lanes.enqueue(l, old);
  lanes.assign(l);
  lanes.enqueue(l, job(2, 'x'));
  const { stopped } = lanes.enqueue(l, job(1, 'b'));
  assert.deepEqual(stopped, [old]);
  assert.equal(old.cancelled, 'superseded by b');
  // The stopping run still holds its slot until it cleans up; then the new head starts.
  assert.deepEqual(lanes.assign(l), []);
  const { replaced } = lanes.enqueue(l, job(1, 'c'));
  assert.deepEqual(replaced.map((j) => j.head), ['b']);
  assert.deepEqual(l.queue.map((j) => j.head), ['x', 'c']);
  lanes.finish(l, l.slots[0]);
  assert.deepEqual(lanes.assign(l).map(({ job: j }) => j.head), ['x']);
});

test('the same head queued again does not stop its own run', () => {
  const l = lanes.createLanes(['2-5']);
  const current = job(1, 'a');
  lanes.enqueue(l, current);
  lanes.assign(l);
  assert.deepEqual(lanes.enqueue(l, job(1, 'a')).stopped, []);
  assert.equal(current.cancelled, undefined);
});

test('a PR with a bench in one slot waits rather than run in a second slot', () => {
  const l = lanes.createLanes(['2-5', '6-9']);
  lanes.enqueue(l, job(1, 'a'));
  lanes.assign(l);
  lanes.enqueue(l, job(1, 'b'));
  lanes.enqueue(l, job(2, 'x'));
  assert.deepEqual(lanes.assign(l).map(({ job: j }) => j.head), ['x']);
  assert.deepEqual(l.queue.map((j) => j.head), ['b']);
});

test('a closed PR or a moved head drops its benches, but not newer ones', () => {
  const l = lanes.createLanes(['2-5', '6-9']);
  const runningOld = job(1, 'a', { queuedAt: 10 });
  lanes.enqueue(l, runningOld);
  lanes.enqueue(l, job(2, 'x', { queuedAt: 11 }));
  lanes.assign(l);
  lanes.enqueue(l, job(3, 'y', { queuedAt: 12 }));
  const closed = lanes.supersede(l, { repo: 'o/r', pr: 1 }, { open: false, asOf: 20 });
  assert.deepEqual(closed.stopped, [runningOld]);
  assert.equal(runningOld.cancelled, 'the PR was closed');
  // GitHub said the head is z, but this bench of w was queued after GitHub was asked: kept.
  lanes.enqueue(l, job(3, 'w', { queuedAt: 30 }));
  const moved = lanes.supersede(l, { repo: 'o/r', pr: 3 }, { open: true, head: 'z', asOf: 25 });
  assert.deepEqual(moved.dropped, []);
  assert.deepEqual(l.queue.map((j) => j.head), ['w']);
  const later = lanes.supersede(l, { repo: 'o/r', pr: 3 }, { open: true, head: 'z', asOf: 40 });
  assert.deepEqual(later.dropped.map((j) => j.head), ['w']);
  // Another repository's PR with the same number is left alone.
  const other = job(2, 'x', { repo: 'o/other' });
  assert.deepEqual(lanes.supersede(l, other, { open: false }).stopped, []);
});

const tick = () => new Promise((r) => setImmediate(r));

test('alone: a measurement waits for the other slots\' work and holds new work off', async () => {
  const lock = lanes.createPhaseLock('alone');
  const events = [];
  let endBuild;
  const build = lock.run('work', () => new Promise((r) => (endBuild = r)).then(() => events.push('build done')));
  await tick();
  let waited = false;
  let endMeasure;
  const measure = lock.run('measure', () => {
    events.push('measure start');
    return new Promise((r) => (endMeasure = r)).then(() => events.push('measure done'));
  }, () => (waited = true));
  const laterBuild = lock.run('work', () => events.push('later build'));
  await tick();
  assert.equal(waited, true);
  assert.deepEqual(events, []);
  endBuild();
  await tick();
  await tick();
  assert.deepEqual(events, ['build done', 'measure start']);
  endMeasure();
  await Promise.all([build, measure, laterBuild]);
  assert.deepEqual(events, ['build done', 'measure start', 'measure done', 'later build']);
  assert.deepEqual(lock.state(), { work: 0, measure: 0, waiting: 0 });
});

test('alone: work runs side by side, measurements one at a time', async () => {
  const lock = lanes.createPhaseLock('alone');
  let builds = 0;
  let most = 0;
  const work = () => lock.run('work', async () => {
    most = Math.max(most, ++builds);
    await tick();
    builds--;
  });
  await Promise.all([work(), work(), work()]);
  assert.equal(most, 3);
  let measuring = 0;
  most = 0;
  const measure = () => lock.run('measure', async () => {
    most = Math.max(most, ++measuring);
    await tick();
    measuring--;
  });
  await Promise.all([measure(), measure()]);
  assert.equal(most, 1);
});

test('parallel: slots measure while others work', async () => {
  const lock = lanes.createPhaseLock('parallel');
  let waited = false;
  let end;
  const build = lock.run('work', () => new Promise((r) => (end = r)));
  await lock.run('measure', async () => {}, () => (waited = true));
  assert.equal(waited, false);
  end();
  await build;
});

test('a failing step releases the lock', async () => {
  const lock = lanes.createPhaseLock('alone');
  await assert.rejects(lock.run('measure', async () => {
    throw new Error('boom');
  }));
  assert.equal(await lock.run('work', async () => 'ok'), 'ok');
});

test('a bench stopped while it waits for the lock leaves the line without running', async () => {
  const lock = lanes.createPhaseLock('alone');
  let end;
  const build = lock.run('work', () => new Promise((r) => (end = r)));
  const stop = new AbortController();
  let ran = false;
  const measure = lock.run('measure', () => (ran = true), null, { signal: stop.signal, aborted: 'cancelled' });
  // Behind it, another slot's build waits too, and goes ahead once the measurement is gone.
  let next = false;
  const later = lock.run('work', () => (next = true));
  await tick();
  stop.abort();
  assert.equal(await measure, 'cancelled');
  await later;
  assert.equal(next, true);
  assert.equal(ran, false);
  end();
  await build;
  assert.deepEqual(lock.state(), { work: 0, measure: 0, waiting: 0 });
  // Already stopped: it never queues.
  assert.equal(await lock.run('work', () => 'ran', null, { signal: AbortSignal.abort(), aborted: 'cancelled' }), 'cancelled');
});
