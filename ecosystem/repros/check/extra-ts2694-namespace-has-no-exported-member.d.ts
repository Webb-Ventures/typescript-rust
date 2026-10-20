// Cause:    extra TS2694: Namespace '…' has no exported member '…'.
// Found in: @types/node@26.6.5 http.d.ts, fs.d.ts (`import * as stream from "node:stream"`,
//           then `stream.Duplex`)
// Owner:    ts_r_checker
// Expected: tsc 7 reports nothing: "node:stream" is `export = Stream` where Stream is a class
//           merged with a namespace, so a namespace import exposes `stream.Duplex`.
// Actual:   ts-r-check reports TS2694 "Namespace 'stream' has no exported member 'Duplex'."
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
