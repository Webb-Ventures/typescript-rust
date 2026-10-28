// Ecosystem check repro (was ecosystem/repros/check/extra-ts2339-function-expando.ts): extra TS2339: Property '…' does not exist on type '…'.
// @filename: index.ts
export function minimatch(): boolean {
  return true;
}
minimatch.sep = '/';

export const sep: string = minimatch.sep;
