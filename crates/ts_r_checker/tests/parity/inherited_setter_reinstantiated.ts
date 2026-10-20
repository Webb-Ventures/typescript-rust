// @filename: dep.ts
// A member inherited through several bases is instantiated twice (`Derived<T>` in the
// dependency's own check, then `Derived<number>`): the setter's type survives both.
export interface Base<T> { get p(): T; set p(v: T | string); }
export interface Other {}
export interface Derived<T> extends Base<T>, Other {}
// @filename: use.ts
import type { Derived } from "./dep";
declare const x: Derived<number>;
x.p = 's';
x.p = true;
