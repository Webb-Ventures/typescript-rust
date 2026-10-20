// Type names print without their namespaces, except to tell apart types that would print
// alike and for an enum type widened from one of its members.
namespace N {
  export interface I { a: number }
  export interface K { a: string }
  export class C { c = 1 }
  export enum E { A, B }
  export namespace Deep {
    export enum DE { X, Y }
    export interface J { j: number }
  }
  export const inside: string = E.A;
}
namespace M {
  export interface I { b: number }
}
export const z: N.I = 1 as any as N.K;
export const z2: N.Deep.J = 1 as any as N.C;
export const z3: N.I = 1 as any as M.I;
export const z4: [N.I, M.I] = 1 as any as string[];
export const z5: N.I | M.I = 1 as any as string;
declare const e: N.E;
e.zzz;
export const s1: string = N.Deep.DE.X;
export const s2: N.Deep.DE.Y = N.Deep.DE.X;
class D extends N.C {
  c = "s";
}
export { D };
