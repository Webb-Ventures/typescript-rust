// node --test scripts/readme-bench.test.mjs
import assert from 'node:assert/strict';
import test from 'node:test';
import { benchTable, replaceSection, section } from './readme-bench.mjs';

const comment = `<!-- ts-r-bench -->
### Bench
**Passed.**
<!-- ts-r-bench:results -->
### Benchmarks (pr @ ac45ecba, linux-x86_64)

| Scenario | Tool | Median | p95 | ± | Throughput | Peak RSS | vs fastest | vs base |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| parse-dom-dts | **ts-r** | pending: no parse-only entry point | | | | | | |
| parse-dom-dts | esbuild | 29.1 ms | 44.5 ms | 5.45 ms | 80.8 MB/s |  | 1.00x | -2.6% |
| version-startup | **ts-r** | 1.17 ms | 1.54 ms | 0.20 ms |  |  | 1.00x | +4.5% |
| version-startup | esbuild | 2.82 ms | 4.06 ms | 0.51 ms |  |  | 2.40x | -6.2% |
| version-startup | tsc-7 | 51.7 ms | 60.0 ms | 6.89 ms |  |  | 44.03x | -3.3% |

### Benchmarks (pr-repos @ ac45ecba, linux-x86_64)

| Scenario | Tool | Median | p95 | ± | Throughput | Peak RSS | vs fastest | vs base-repos |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| lint-trpc | **ts-r-lint** | 629.8 ms | 700 ms | 41.3 ms |  |  | 20.92x | -3.5% |
| lint-trpc | eslint | 10851.9 ms | 11000 ms | 100 ms |  |  | 360x | -1% |
| check-rxjs | **ts-r-check** | 791.3 ms | 813.8 ms | 15.4 ms |  |  | 8.90x | -34.4% |
| check-rxjs | **ts-r-check-warm** | 89.0 ms | 102.3 ms | 7.08 ms |  |  | 1.00x | -17.5% |
| check-rxjs | tsc-7 | 5529.9 ms | 5590.0 ms | 140.7 ms |  |  | 62.16x | -0.9% |
| build-three-x10 | **ts-r-build** | 1077.3 ms | 1244.4 ms | 68.9 ms |  |  | 1.61x | -2.9% |
| build-three-x10 | esbuild | 1018.1 ms | 1067.2 ms | 35.3 ms |  |  | 1.52x | -1.2% |
`;

test('one row per like-for-like scenario, against the fastest other tool', () => {
  const table = benchTable(comment, '2026-10-20');
  const rows = table.split('\n').filter((l) => l.startsWith('| `'));
  assert.deepEqual(rows, [
    '| `version-startup` | `ts-r` 1.17 ms | `esbuild` 2.82 ms | **2.4× faster** |',
    '| `check-rxjs` | `ts-r-check` 791 ms | `tsc-7` 5.53 s | **7.0× faster** |',
    '| `build-three-x10` | `ts-r-build` 1.08 s | `esbuild` 1.02 s | 1.1× slower |',
  ]);
  assert.match(table, /on linux-x86_64/);
  assert.match(table, /measured 2026-10-20/);
});

test('no comparable rows gives null', () => {
  assert.equal(benchTable('<!-- ts-r-bench:results -->\nnothing'), null);
});

test('sections are read and replaced between their markers', () => {
  const readme = 'a\n<!-- bench:main:start -->\nold\n<!-- bench:main:end -->\nb\n<!-- bench:release:start -->\n<!-- bench:release:end -->\n';
  assert.equal(section(readme, 'main'), 'old');
  const next = replaceSection(readme, 'release', 'new');
  assert.equal(section(next, 'release'), 'new');
  assert.equal(section(next, 'main'), 'old');
  assert.throws(() => replaceSection('no markers', 'main', 'x'));
});
