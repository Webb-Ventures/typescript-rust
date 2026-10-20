// Cause:    extra TS2339: Property '…' does not exist on type '…'.
// Found in: @types/node@26.6.5 (`[Symbol.dispose]()` / `[Symbol.asyncDispose]()` in fs, dgram,
//           child_process; same root cause as extra-ts2304-cannot-find-name.d.ts)
// Owner:    ts_r_checker
// Expected: tsc 7 loads lib.esnext.disposable.d.ts for the `<reference lib>` below, so
//           SymbolConstructor has `dispose`; no error.
// Actual:   ts-r-check reports TS2339 "Property 'dispose' does not exist on type
//           'SymbolConstructor'."
/// <reference lib="esnext.disposable" />
export declare class Handle {
  [Symbol.dispose](): void;
}
