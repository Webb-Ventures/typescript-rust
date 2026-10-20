// @target: es2022
import type { T } from "./t";
import { a, type B, unused } from "./a";
import def from "./def";
interface I { x: number }
type U = string | number;
declare const g: number;
function over(x: string): void;
function over(x: any) { return x as any; }
export function f<T>(this: Window, x: T, y?: number): T { return x!; }
const v = <number>(g satisfies number);
let w!: string;
abstract class Q<T> implements I {
  x: number = 1;
  declare y: string;
  private z?: string;
  abstract m(): void;
  constructor(public p: number, readonly q = 2) { super(); }
  static s: number;
  get k(): number { return 1 }
}
console.log(a, f<string>("x"), (v as any).b, (a?.b)!.c);
export default def;
export type { U };
