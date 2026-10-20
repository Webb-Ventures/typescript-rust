class Foo {
    static enumMember = Enum.A;
    static objLiteralMember = ObjLiteral.A;
    static namespaceMember = Namespace.A;
    static fnMember = Namespace.f;
    instEnum = Enum.A;
    instObj = ObjLiteral.A;
    static later = After.data;
    static laterMethod = After.method;
    static earlier = Before.data;
    static nested = Namespace.Inner.B;
    static { Enum.A; ObjLiteral.A; }
}
class Before { static data = 1; }
const top = Enum.A;
function fn() { return Enum.A; }
enum Enum {
    A
}
const ObjLiteral = {
    A: 0
};
namespace Namespace {
    export let A = 0;
    export function f() {}
    export namespace Inner { export const B = 1; }
}
class After {
    static data = 12;
    static method() { }
}
class G {
    static a = G2.x;
}
class G2 { static x = 1; }
export {};
