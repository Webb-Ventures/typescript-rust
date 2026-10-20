// Cause:    extra TS2503: Cannot find namespace '…'.
// Found in: @types/node@26.6.5 test.d.ts:450 (`interface Error extends globalThis.Error`)
// Owner:    ts_r_checker (name resolution)
// Expected: tsc 7 resolves `globalThis` as a namespace in a type position (`globalThis.Error`
//           is the global Error interface), so the inner `Error` can extend the outer one.
//           No error.
// Actual:   ts-r-check reports TS2503 "Cannot find namespace 'globalThis'."
export declare namespace EventData {
  interface Error extends globalThis.Error {
    cause: unknown;
  }
}
