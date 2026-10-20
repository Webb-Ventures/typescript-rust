// Generic source signatures: one declaration's instantiations relate with type parameters
// identified first; otherwise type arguments are inferred, then taken by position.
interface Box<O, I> { o: O; i: I }
interface Pp<A, B> { a: A; b: B }
type Out<X> = X extends { v: infer V } ? V : never;

// Two instantiations of one declared method share their type parameters.
interface Base { m<T extends Box<any, Out<this>>>(x: T | Box<any, Out<this>>): Pp<this, T>; v: unknown }
interface NumB extends Base { v: number }
declare const b1: NumB['m'];
export const b2: Base['m'] = b1;
interface G<V> { m<T extends Box<any, V>>(x: T | Box<any, V>): Pp<V, T>; v: V }
declare const d1: G<number>['m'];
export const d2: G<unknown>['m'] = d1;

// A separately declared copy infers and falls back to the constraint.
interface Other { m<T extends Box<any, unknown>>(x: T | Box<any, unknown>): Pp<Other, T>; v: unknown }
export const b3: Other["m"] = d1;

// Type parameters are inferred, not matched by position.
interface El { tag: string }
type Fn<On, Arg, R> = string | ((on: On, arg: Arg) => R);
interface Src { f<R, Arg>(fn: Fn<El, Arg, R>, arg?: Arg): void }
interface Tgt { f<Arg, E extends El>(fn: Fn<E, Arg, any>, arg?: Arg): void }
declare const src: Src;
export const tgt: Tgt = src;

// Identical constraints: the same type parameters.
abstract class Storage1 { abstract get<T extends string>(): T | Promise<T>; }
export abstract class Sync1 extends Storage1 { abstract override get<T extends string>(): T; }
