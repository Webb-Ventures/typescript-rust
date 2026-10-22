// A generic function passed where a non-generic function is expected, to a call returning
// a non-generic function, keeps its type parameters: they become type parameters of the
// returned function (tsc's higher-order function type inference), renamed when taken.
declare function pipe<A extends any[], B, C>(
  ab: (...args: A) => B,
  bc: (b: B) => C,
): (...args: A) => C;
declare function pipe1<A extends any[], B>(ab: (...args: A) => B): (...args: A) => B;
declare function list<T>(a: T): T[];
declare function box<V>(x: V): { value: V };
declare function pair<T>(a: T, b: T): [T, T];

const f1: <T>(a: T) => T[] = pipe1(list);
const f2: <T>(a: T) => { value: T[] } = pipe(list, box);
const f3: <T>(a: T, b: T) => [T, T][] = pipe(pair, list);
const n1: number = pipe1(list);
const n2: number = pipe(list, list);

// A generic contextual return type infers its own type parameters as types.
declare function compose<A, B, C>(f: (b: B) => C, g: (a: A) => B): (a: A) => C;
declare function len(s: string): number;
const f4: <T extends string>(x: T) => number = compose(len, (x) => x);

// With every type parameter the context needs already inferred (here from the contextual
// return type) a generic argument is instantiated in that context instead.
interface Box<T> {
  value: T;
}
declare function chain<A, B, C>(f: (a: A) => B, g: (b: B) => C): (a: A) => C;
declare function unbox<W>(x: Box<W>): W;
declare function unlist<T>(a: T[]): T;
const f5: <T>(x: Box<T[]>) => T = chain(unbox, unlist);
const f6: (x: Box<string[]>) => string = chain(unbox, unlist);
const f7: (x: Box<string[]>) => number = chain(unbox, unlist);
