// A `/// <reference lib>` in a program file adds that lib; merged `declare module` blocks see
// each other's exports; `globalThis.X` resolves global types.
// @filename: types.d.ts
/// <reference lib="esnext.disposable" />
declare module 'lib' {
    export interface Opts { a: number }
}
declare module 'lib' {
    export function make(o: Opts): Disposable;
}
type G = globalThis.Array<number>;
type Missing = globalThis.Nope;
declare const g: G;
// @filename: a.ts
import { make } from 'lib';
const d = make({ a: 1 });
d[Symbol.dispose]();
const s: Disposable = d;
const n: string = g[0];
// @filename: aug.d.ts
export {};
declare class Foo { a(): void }
interface Foo extends globalThis.Bar {}
declare global {
  interface Bar { b(): void }
  interface Baz { f(x: Foo): void; g(x: Qux): void }
  var z: Baz;
  var Foo: number;
}
declare class Qux { q(): void }
interface Qux extends globalThis.Bar {}
// @filename: g.d.ts
interface Foo { a(): void }
// @filename: c.ts
z.f({ a() {} });
z.g({ q() {} });
