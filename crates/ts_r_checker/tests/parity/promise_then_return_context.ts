// `promise.then(() => ({ done: false, value }))` where an iterator result (or a promise of
// one) is expected: the return type `Promise<TResult1 | TResult2>` passes the expected
// type to both naked type parameters, so the callback's object literal keeps `done: false`.
declare const RESOLVED: Promise<void>;
type R<T> = { done: false; value: T } | { done: true; value: void };
function f<T>(values: T[], cb: (value: R<T[]> | PromiseLike<R<T[]>>) => void) {
  cb(RESOLVED.then(() => ({ done: false, value: values })));
}
function g(values: number[], cb: (value: IteratorResult<number[], void> | PromiseLike<IteratorResult<number[], void>>) => void) {
  cb(RESOLVED.then(() => ({ done: false, value: values })));
}
const p1: Promise<R<number>> = RESOLVED.then(() => ({ done: false, value: 1 }));
const p2: PromiseLike<R<number>> = RESOLVED.then(() => ({ done: false, value: 1 }));
declare function both<A, B = never>(cb: () => A | PromiseLike<A>): Promise<A | B>;
const p3: Promise<R<number>> = both(() => ({ done: false, value: 1 }));
