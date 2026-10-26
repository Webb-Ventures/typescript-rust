// @target: es2015
// @module: commonjs
namespace M {
    export namespace N {}
    namespace T { export interface I {} }
    export namespace V { export const v = 1; }
    export import X = N;
    export import Y = T;
    export import Z = V;
    export import W = T.I;
}
namespace A { export interface I { x: number } }
namespace B { export import A1 = A; }
namespace a { export type A = number; }
namespace b { export import A = a.A; export namespace A {} }
namespace q { export const Q = {}; }
namespace r { export import Q = q.Q; export type Q = number; }
export namespace TT { export interface I {} }
export import TA = TT;
export import TV = M.V;
