// Ecosystem check repro (was ecosystem/repros/check/extra-ts2339-property-does-not-exist-on-type.d.ts): extra TS2339: Property '…' does not exist on type '…'.
// @filename: index.d.ts
/// <reference lib="esnext.disposable" />
export declare class Handle {
  [Symbol.dispose](): void;
}
