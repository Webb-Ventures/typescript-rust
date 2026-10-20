// @target: es2022
// @module: esnext
import { type A, type B } from "./types";
import Def, * as NS from "./ns";
import { used } from "./used";
import { onlyType } from "./only-type";
export type { A };
export { type B };
let x: onlyType;
declare module "foo" { export const x: number; }
declare global { interface Window { y: number } }
declare namespace Amb { const z: number; }
abstract class K { abstract a: number; protected abstract b(): void; }
function g(this: K, a?: number, ...rest: any[]): asserts a is number {}
const h = <T,>(v: T): T => v;
const i = async <T>(v: T): Promise<T> => v;
let j = used as unknown as string[];
let k = (used satisfies any)!;
let l = (used?.x as any).y;
let m = (<any>used).z;
for (const q of [] as number[]) {}
enum E { A = 1 }
export { E };
type Gen<T> = T;
const gen = h<Gen<number>>;
class Impl<T> extends Array<T> implements Iterable<T> { override push(...items: T[]): number { return super.push(...items); } }
export {};
