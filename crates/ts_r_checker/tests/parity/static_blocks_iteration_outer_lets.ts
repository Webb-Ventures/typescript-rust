// `super` in a static block refers to the base constructor.
declare class Base {
    static method(): void;
    static a: number;
}
class Derived extends Base {
    static {
        super.method();
        super.a;
    }
}

// A static block runs inline: its assignments and narrowings flow through it.
let foo: number;
class S1 {
    static {
        foo = 1;
    }
}
foo;
let early: number;
class S2 {
    static {
        early;
    }
}
early = 1;
let wide: number | string = 1;
class S3 {
    static {
        const n: number = wide;
    }
}
wide = "";

// `any` anywhere along the iterator protocol makes the element `any`.
declare var anyValue: any;
class AnyIterator {
    [Symbol.iterator]() {
        return anyValue;
    }
}
for (const v of new AnyIterator()) { }
class AnyNext {
    next: any;
    [Symbol.iterator]() {
        return this;
    }
}
for (const v of new AnyNext()) { }

// `for await` accepts sync iterables and unions with async ones.
declare const syncIterable: Iterable<number>;
declare const promises: Iterable<Promise<string>>;
async function iterate<T>(source: Iterable<T> | AsyncIterable<T>) {
    for await (const x of source) { }
    for await (const n of syncIterable) {
        const k: number = n;
    }
    for await (const s of promises) {
        const t: string = s;
    }
    for await (const bad of {}) { }
}

// A method's computed name is read from the method's own flow container.
var unassignedVar: string;
const o1 = { [unassignedVar]() { } };
class M1 { [unassignedVar]() { } }
class M2 { [unassignedVar] = 1; }
const o2 = { [unassignedVar]: 1 };

// An outer local `let` that is never assigned is still reported in closures.
let neverAssigned: number;
function readsNever() {
    return neverAssigned;
}
let assignedLater: number;
function readsLater() {
    return assignedLater;
}
assignedLater = 1;

// Computed names in interfaces and type literals are not reads.
let keyName: string;
interface WithKey { [keyName](): void }
declare let withKey: { [keyName]: number };

export {};
