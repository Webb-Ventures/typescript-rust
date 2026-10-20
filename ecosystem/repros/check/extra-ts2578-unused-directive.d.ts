// Cause:    extra TS2578: Unused '…' directive.
// Found in: postcss@8.5.30 lib/postcss.d.mts:40 (`// @ts-expect-error` above `plugin` in
//           `export { ... } from './postcss.js'`, where postcss.d.ts is `export = postcss`)
// Owner:    ts_r_checker (re-exports from an `export =` module)
// Expected: tsc 7 reports TS2305 for `plugin` (the `export =` namespace has no such member),
//           which the `@ts-expect-error` suppresses. No error.
// Actual:   ts-r-check finds no error on `plugin`, so it reports TS2578 "Unused
//           '@ts-expect-error' directive."
// @filename: index.d.ts
export {
  // @ts-expect-error This value exists, but it's untyped.
  plugin,
  Plugin,
} from './lib';
// @filename: lib.d.ts
declare function lib(): void;
declare namespace lib {
  interface Plugin {
    name: string;
  }
}
export = lib;
