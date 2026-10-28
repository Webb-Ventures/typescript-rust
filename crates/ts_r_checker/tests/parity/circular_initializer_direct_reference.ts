// An unannotated variable whose initializer names it outside its functions is `any` (and
// TS7022), even when the initializer also has functions: `b.foo()` reads `b` while `b`'s
// type is being computed, so it is not called with `b`'s final type.
var b = {
    foo: <T extends Date>(x: T): T => x,
    bar: b.foo().notHere()
};
const c = { f: () => c.g, g: 1 };
const d = { f() { return 1 }, g: d.f() };
const n: number = c.f();
