// An alias of a type read as a value is skipped: the lookup goes on outwards, here to the
// namespace `A.M` itself (dotted namespaces see their parents' exports).
namespace Z {
    export namespace M {
        export function bar() {
            return "";
        }
    }
    export interface I { }
}
namespace A.M {
    import M = Z.I;
    export function bar() {
    }
    M.bar();
}
