// trpc `fastifyTRPCPlugin.ts` (`/// <reference types="@fastify/websocket" />`): an augmentation
// of a module whose `export =` is a namespace merges with the declarations that namespace
// re-exports (`export type { FastifyInstance }`), keeping their type parameter defaults.
// @filename: node_modules/fast/types/instance.d.ts
export interface Inst<A = string, B extends number = 1> {
  a: A;
  b: B;
  get(path: string, opts: RouteOpts): void;
}
export interface RouteOpts<S = string> {
  url?: S;
}
// @filename: node_modules/fast/index.d.ts
import { Inst, RouteOpts } from './types/instance'
declare namespace fast {
  export type { Inst, RouteOpts }
}
declare function fast(): Inst;
export = fast
// @filename: node_modules/fast/package.json
{ "name": "fast", "version": "1.0.0", "types": "index.d.ts" }
// @filename: node_modules/fastws/index.d.ts
import * as fast from 'fast'
declare module 'fast' {
  interface RouteOpts<S = string> {
    websocket?: boolean;
  }
  interface Inst<A, B> {
    ws: A;
  }
}
export {}
// @filename: node_modules/fastws/package.json
{ "name": "fastws", "version": "1.0.0", "types": "index.d.ts" }
// @filename: a.ts
/// <reference types="fastws" />
import type { Inst, RouteOpts } from 'fast';
export function f(i: Inst) {
  i.get('/', { websocket: true });
  const n: null = i.ws;
  const r: RouteOpts = { websocket: true, url: 1 };
}
