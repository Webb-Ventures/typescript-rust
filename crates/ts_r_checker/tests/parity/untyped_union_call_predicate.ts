declare const f: Function | ((v: unknown) => v is string);
declare const x: unknown;
if (f(x)) { x.toUpperCase(); }
declare const typed: (v: unknown) => v is string;
if (typed(x)) { x.toUpperCase(); }
declare const assert: Function | ((v: unknown) => asserts v is string);
assert(x);
x.toUpperCase();
declare const g: Function | ((v: number) => v is 1);
g("x");
