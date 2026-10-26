// In the true branch of `T[K] extends unknown[] ? (...args: T[K]) => void : never`,
// `T[K]` reads as `unknown[] & T[K]`; once `T[K]` is a tuple that satisfies `unknown[]`
// it is the tuple alone (`@types/node` EventEmitter listeners, Playwright's Recorder).
type L<K, T> = K extends keyof T ? (T[K] extends unknown[] ? (...args: T[K]) => void : never) : never;
type M = { p: [info: string, gesture?: boolean]; o: [] };
declare const a: L<'p', M>;
const x1: number = a;
declare function on<K>(k: K | keyof M, l: L<K, M>): void;
on('p', (info: string) => {});
on('o', () => {});
type W<T> = T extends string ? { v: T } : never;
declare const w: W<'a'>;
const x2: number = w;
type N<T> = T extends unknown[] ? (...args: T) => void : never;
declare const n: N<[string] | number[]>;
const x3: number = n;
export {};
