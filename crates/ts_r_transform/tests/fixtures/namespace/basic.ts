// @target: es2022
namespace N {
  export const a = 1, b2 = 2;
  export let unset: number;
  export function f() { return a + b; }
  const b = a;
  export class C { m() { return N.a; } }
  export namespace M { export const c = b; }
  namespace Inner { export var d = a; }
  export enum E { X }
  export const { p, q: [r] } = { p: 1, q: [2] };
  export import Al = M.c;
  export interface I {}
  export type T = number;
  export declare const amb: number;
  a;
  function g() { return unset; }
}
namespace N { export const late = a + f(); }
namespace A.B.C { export const deep = 1; }
namespace TypesOnly { export interface X {} export type Y = 1; }
function Merged() {}
namespace Merged { export const extra = 1; }
class MC {}
namespace MC { export const s = 2; }
export namespace Exp { export const e = 1; }
declare namespace Amb { const z: number; }
