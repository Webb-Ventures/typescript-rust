// Use before declaration: own initializers, IIFEs, decorators, static blocks, enums.
declare const dec: any;

// A block-scoped variable read in its own initializer.
const a: number = a + 1;
const b = { f: () => b };
const c: number = (() => c)();
let d: number = ((x = d) => x)();

// Class decorators run before the class exists; deferred reads are fine.
@dec(C1) class C1 {}
@dec(() => C2) class C2 {}

class C3 {
    @dec(C3) m() {}
    @dec(() => C3) n() {}
    @dec(C3) p: any;
    static s = C3;
    static {
        C3.s;
    }
}

// A computed member name reads the class binding before it exists.
class C4 {
    static [C4.name] = 1;
    [String(C4)]() {}
}

// An enum member may refer to an earlier member through the enum name.
enum E {
    A = 1,
    B = E.A + 1,
}

// Decorator expressions are checked like any expression.
@dec(missing) class C5 {}

// A generator's class decorator may yield.
function* g() {
    @(yield dec) class C6 {}
}
