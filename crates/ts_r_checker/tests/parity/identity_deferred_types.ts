// Identity (expect-type's `toEqualTypeOf`) compares deferred types by their parts and two
// references to one generic type by their arguments (RxJS `Observable<T>` specs).
type Eq<L, R> = (<T>() => T extends (L & T) | T ? true : false) extends <T>() => T extends (R & T) | T ? true : false ? true : false;
interface Info { timestamp: number; elapsed: number }
type B = { timestamp: number; elapsed: number };
type F1<T> = <K extends keyof T>(key: K, c?: (a: T[K]) => void) => T;
type F2<T> = <R extends T>(p: (v: T) => v is R) => Exclude<T, R>;
type F3<T> = <R>(p: (v: T) => R) => Exclude<T, R>;
type F4<T> = <R>(p: (v: T) => R) => R extends T ? 1 : 2;
interface Obs<T> {
  drop(n: number): Obs<T>;
  skipWhile: F2<T>;
  key: F1<T>;
}
export const a: Eq<F1<Info>, F1<B>> = true;
export const b: Eq<F2<Info>, F2<B>> = true;
export const c: Eq<F3<Info>, F3<B>> = true;
export const d: Eq<F4<Info>, F4<B>> = true;
export const e: Eq<Obs<Info>, Obs<B>> = true;
export const f: Eq<F4<Info>, F4<{ timestamp: number }>> = false;
export const g: Eq<Obs<Info>, Obs<{ elapsed: number }>> = false;
