// Ecosystem check repro (was ecosystem/repros/check/extra-ts2578-unused-directive.d.ts): extra TS2578: Unused '…' directive.
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
