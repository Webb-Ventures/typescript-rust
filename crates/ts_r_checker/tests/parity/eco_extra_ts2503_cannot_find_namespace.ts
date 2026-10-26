// Ecosystem check repro (was ecosystem/repros/check/extra-ts2503-cannot-find-namespace.d.ts): extra TS2503: Cannot find namespace '…'.
// @filename: index.d.ts
export declare namespace EventData {
  interface Error extends globalThis.Error {
    cause: unknown;
  }
}
