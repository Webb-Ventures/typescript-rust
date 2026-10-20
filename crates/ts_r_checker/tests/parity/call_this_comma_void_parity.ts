// Calls on `Function | (() => T)` check arguments against the callable members and are `any`.
declare const c: Function | (() => object);
const r = c();
r.foo;
declare const d: Function | ((x: number) => string);
d("a");
declare const g: Function | { (a: string): 1; (a: number): 2 } | { (a: boolean): 3; (a: 1): 4 };
g(1);
declare const e: Function | string;
e();

// `(f(), x).p` is the reference `x.p`.
declare const value: { inner: number | string };
declare function other(): void;
if (typeof (other(), value).inner === "number") {
    const b: number = (other(), value).inner;
}

// An explicit `this` parameter narrows.
declare function isBig(x: any): x is { big: true };
function bigger(this: {}) {
    if (isBig(this)) {
        this.big;
    }
}
function bar(this: string | number) {
    if (typeof this === "string") {
        const x: string = this;
    }
}

// `void | undefined` keeps both members; a union with `void` prints unwidened.
declare let u: undefined | void | 1;
const n: number = u;
declare function isDefined<T>(value: T | undefined | null | void): value is T;
declare const foo: string | undefined;
if (isDefined(foo)) {
    foo.toUpperCase();
}
