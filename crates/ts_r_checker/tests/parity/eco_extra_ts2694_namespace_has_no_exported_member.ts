// Ecosystem check repro (was ecosystem/repros/check/extra-ts2694-namespace-has-no-exported-member.d.ts): extra TS2694: Namespace '…' has no exported member '…'.
// @filename: index.d.ts
/// <reference path="stream.d.ts" />
import * as stream from 'node:stream';
export declare function connect(): stream.Duplex;
// @filename: stream.d.ts
declare module 'node:stream' {
  class Stream {
    pipe(): void;
  }
  namespace Stream {
    class Duplex extends Stream {}
  }
  export = Stream;
}
