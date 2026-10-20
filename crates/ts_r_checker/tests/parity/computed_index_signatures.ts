// Members whose computed key has a non-literal type imply index signatures: `string` over
// string- and number-keyed members, `number` over number-keyed ones, `symbol` over
// symbol-keyed ones. A static `string` index also reads `prototype` (the instance type).
declare var s: string;
declare var y: symbol;
declare var n: number;
declare var prefixed: `a${string}`;
var notUnique = Symbol.for("x");
class C {
    [s]: number = 1;
    [y]() { return 0; }
    static [s] = "x";
    [n]: boolean = true;
    readonly [prefixed] = 1n;
}
interface I { [s]: number; }
type T = { [s]: boolean };
declare const c: C;
declare const i: I;
declare const t: T;
const a1: string = c["x"];
const a2: string = c[y];
const a3: string = C["x"];
const a4: string = i["x"];
const a5: string = t["x"];
const a6: string = c[1];
c["q"] = 1;

// They do not constrain the other members.
class D {
    [s]: number = 0;
    x: string = "";
}

// A `var` initialized with `Symbol.for` is a plain `symbol`.
class Foo {
    [notUnique]() {
        return 0;
    }
}
class Bar extends Foo {
    [notUnique]() {
        return super[notUnique]();
    }
}
export {};
