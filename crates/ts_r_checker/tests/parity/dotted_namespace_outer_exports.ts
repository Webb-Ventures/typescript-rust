// `namespace A.B { }` is inside `A`: the exports of A's other declarations are in scope.
namespace A {
    export var a1 = 1;
}
namespace A.B {
    export var b1 = a1;
}
namespace A.C {
    var c1: number = B.b1;
    var c2: string = a1;
}
function foo(title: string) {}
namespace foo.Bar {
    export function f() {}
}
namespace foo.Baz {
    export function g() { Bar.f(); }
}
declare namespace E {
    export interface I { x: number }
}
namespace E.T.U {
    export class X<T extends I> {}
}
// `yield` outside a generator: TS1163 only, its operand is not checked.
class K {
    constructor() {
        yield missing;
    }
}
