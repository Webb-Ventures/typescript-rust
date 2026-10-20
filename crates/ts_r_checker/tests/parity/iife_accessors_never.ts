// Object literal methods that can't return infer `never`, like function expressions.
const o = { m() { throw new Error("x"); } };
const notNumber: number = o.m;
interface I { m(): number; }
const throws: I = { m() { throw new Error("not implemented"); } };

// An immediately invoked function returns into its call's context.
const test1: Promise<[one: number, two: string]> = (async () => {
    return [1, "two"];
})();
const obj: { foo: [one: number, two: string] } = {
    foo: (() => [1, "two"])(),
};
const nested: (v: any) => string = (function () { return function () { return function (v) { return v; }; }(); })();
const wrongIife: string = (() => 1)();

// A setter with a computed name takes its parameter type from the matching getter.
const spread = {
    get [Symbol.isConcatSpreadable]() { return false; },
    set [Symbol.isConcatSpreadable](x) { const b: boolean = x; },
};

// Writes to a divergent accessor take the setter's parameter type.
class Test1 {
    get foo(): string { return ""; }
    set foo(s: string | number) {}
}
interface Test2 {
    get foo(): string;
    set foo(s: string | number);
}
declare const t2: Test2;
const t1 = new Test1();
t1.foo = 32;
t2.foo = 32;
t1.foo = true;
t2["foo"] = false;
const read: number = t1.foo;

export { notNumber, throws, test1, obj, nested, wrongIife, spread, read };
