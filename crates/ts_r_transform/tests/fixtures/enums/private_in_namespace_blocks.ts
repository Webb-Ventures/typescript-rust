// @target: es2022
// @module: esnext
// Enums that are not exported are local to their namespace block: two blocks declaring a
// private `E` have two enums, and `N.a` inlines the first block's `E.A`.
export namespace N { export const a = E.A; const enum E { A = 1 } }
export namespace N { const enum E { A = 2 } export const b = E.A; }
export namespace M { export const a = E.A; enum E { A = 1 } }
export namespace M { enum E { A = 2 } export const b = E.A; }
