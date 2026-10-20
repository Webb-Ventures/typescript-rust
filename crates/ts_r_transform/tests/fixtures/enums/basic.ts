// @target: es2022
declare function foo(): number;
enum E { A, B = A << 2, C, D = "d", E = `e`, F = -1, G = foo(), H = G + 1, I = 1 + 2 * 3, "quoted key" = 9, J = ~5, K = 2 ** 3 }
export enum E2 { X = 10, Y, Z = X | Y }
enum E { L = 100 }
const enum CE { X = 1, Y = X + 1, Z = "zz" }
let a = CE.Y, b = CE["Z"], c = E.A;
declare enum DE { Q }
function inner() { enum Local { A } return Local.A; }
export const enum ECE { A }
