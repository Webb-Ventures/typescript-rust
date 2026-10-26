// @target: esnext
namespace A { const A = 0; }
namespace B { export function B() {} }
namespace C { export namespace C { export const x = 1; } }
namespace D { export const x = 1; }
namespace E { export const E = 1; }
namespace E { export const y = E; }
namespace F { export enum F { a } }
namespace G { export class G {} }
namespace I { interface I {} export const x = 1; }
namespace T { type T = 1; export const x = 1; }
namespace V { var V = 1; }
namespace W { { let W = 1; } export const x = 1; }
namespace A { export const A = 0; }
namespace Z { export const A = 0; }
namespace A { export const B = A; }
namespace A { const A = 1; }
namespace A.A { export const q = 1; }
namespace P.Q { export const Q = 1; }
namespace F2 { function f(F2: any) {} }
namespace F3 { function f() { let F3; } }
namespace F4 { export class C { F4() {} } }
namespace E2 { export enum Q { E2 } }
namespace G2 { function f<G2>() {} }
namespace C2 { try {} catch (C2) {} }
namespace L2 { const f = (L2: any) => 1; }
namespace M2 { import type M2 = Y; }
namespace Y { export const z = 1; }
namespace X { import X = Y.z; export const x = X; }
namespace K { declare const K: number; export const x = K; }
namespace N1.A.B { namespace A {} export class W {} }
namespace N2 { namespace N2 { export interface I {} } export const x = 1; }
namespace N3 { namespace Q { namespace N3 { export const y = 1; } } }
