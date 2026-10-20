// `x!` of a type parameter whose constraint may be nullish is `T & {}`, related through the
// constraint without `null` and `undefined`.
declare function takesString(p: string): void;
function nonNull<T extends string | undefined, U extends string, V extends number | null>(
    one: T,
    two: U,
    three: V,
) {
    let either = Boolean() ? one : two;
    takesString(one!);
    takesString(two!);
    takesString(either!);
    takesString(three!);
}

// Arrays, tuples and `number`-indexed objects report other keys as TS7015.
interface NumberIndexed { [n: number]: string; x: 1 }
declare const numberIndexed: NumberIndexed;
declare const tuple: [number, string];
declare const someString: string;
declare const someSymbol: symbol;
const viaLiteral = numberIndexed["k"];
const viaString = numberIndexed[someString];
const viaSymbol = numberIndexed[someSymbol];
const tupleLiteral = tuple["k"];
const arrayLiteral = ([] as number[])["k"];

// Union members whose overloads (or generic signatures) are identical are callable; the
// result is the union of the return types.
interface Callable<Name extends string> {
    (): `${Name} without id`;
    (id: number): `${Name} with id`;
}
declare const callable: Callable<"A"> | Callable<"B">;
const withId: number = callable(123);
const withoutId: number = callable();
class A { t!: number; get<K extends "t">(k: K): A[K] { return this[k]; } }
class B { t!: string; get<K extends "t">(k: K): B[K] { return this[k]; } }
declare const ab: A | B;
const got: boolean = ab.get("t");

// A tuple default shorter than its array pattern is padded with optional elements.
function padded([x, y = 0] = [0]) { }
function allDefaults([x = 0, y = 0] = []) { }
padded([1, 1]);
allDefaults([1, 1]);
const paddedType: string = padded;

// Computed keys: generic keys within `string | number | symbol` and `never` are valid; an
// `any` key implies a `number` index signature.
function keys<K extends keyof U, U, S extends string, T>(k: K, s: S, t: T, n: never) {
    return [{ [k]: 1 }, { [s]: 1 }, { [t]: 1 }, { [n]: 1 }];
}
declare const anyKey: any;
interface WithAny { [anyKey]: "x"; }
interface ExtendsAny extends WithAny { [anyKey]: "y"; }
declare const withAny: WithAny;
const byNumber: "z" = withAny[1];
const byString = withAny["k"];

// `public two?: U` declares an optional property.
class Optional<T, U> { constructor(public one: T, public two?: U) { } }
const fromLiteral: Optional<number, string> = { one: 1 };
export {};
