// A generic function argument instantiated in its contextual signature's context gives a
// type parameter that no parameter mentions its default, constraint or `unknown`: the
// argument's own type parameters do not leak into the call's inferences.
declare const pair: <T, S>(x: T) => (y: S) => { x: T; y: S };
declare const zipWith: <T, S, U>(a: T[], b: S[], f: (x: T) => (y: S) => U) => U[];
const result = zipWith([1, 2], ["a", "b"], pair);
const i: number = result[0]!.x;
const j: string = result[0]!.y;

declare const make: <T, S extends string = "s">(x: T) => () => S;
declare function apply<A, R>(a: A, f: (a: A) => () => R): R;
const k: "s" = apply(1, make);
const l: number = apply(1, make);
