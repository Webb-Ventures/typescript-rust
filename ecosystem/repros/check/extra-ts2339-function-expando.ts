// Cause:    extra TS2339: Property '…' does not exist on type '…'.
// Found in: minimatch@10.2.6 src/index.ts:212-316 (tests stage: its `tsc` build),
//           `minimatch.sep = sep`, `minimatch.GLOBSTAR = GLOBSTAR`, ...
// Owner:    ts_r_checker (binder: expando assignments on function declarations)
// Expected: tsc 7 treats `f.x = ...` after `function f` (same file, top level) as declaring a
//           property on f's type ("expando"). No error.
// Actual:   ts-r-check reports TS2339 "Property 'sep' does not exist on type '() => boolean'."
export function minimatch(): boolean {
  return true;
}
minimatch.sep = '/';

export const sep: string = minimatch.sep;
