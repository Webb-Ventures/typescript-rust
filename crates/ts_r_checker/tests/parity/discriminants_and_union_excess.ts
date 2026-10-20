// Assertions and predicates on a discriminant narrow the object.
interface Cat { type: "cat"; canMeow: true }
interface Dog { type: "dog"; canBark: true }
type Animal = Cat | Dog;
declare function assertEqual<T>(value: any, type: T): asserts value is T;
declare const animal: Animal;
assertEqual(animal.type, "cat" as const);
animal.canMeow;
declare const maybe: Animal | undefined;
assertEqual(maybe?.type, "cat" as const);
maybe.canMeow;

// A template key without substitutions is a discriminant.
type U = { kind: "A"; a: number } | { kind: "B"; b: string };
function t(val: U) {
    if (val[`kind`] === "B") {
        return val.b;
    }
    return val.a;
}

// A union member without the discriminant stays a candidate.
type Arg = { mode: "numeric"; data: number } | { mode: "alphabetic"; data: string } | { data: string | Uint8Array };
declare const u8: Uint8Array;
const a1: Arg = { mode: "numeric", data: u8 };
const a2: Arg = { mode: "numeric", data: true };

// Object literal properties check against the union's property types.
interface IValue { value: string }
interface StringKeys { [k: string]: IValue }
interface NumberKeys { [k: number]: IValue }
const d1: StringKeys | NumberKeys = { foo: "x" };
const d2: { [x: string]: number } | { a: number } = { a: 5, c: "abc" };
type Props = { [key: string]: never } | { p1: string } | { p2: string };
declare const p2: string | undefined;
const d3: Props = { p2 };
type Thing = number | { toFixed: null } | object;
const d4: Thing = { toString: null };

// Excess property checks only use the non-generic members.
function g<T extends { name: string }>() {
    const o3: T | { prop: boolean } = { name: "test", prop: true };
}
