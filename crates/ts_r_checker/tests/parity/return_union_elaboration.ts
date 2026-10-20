// Return errors sit on the `return` keyword unless elaborated; conditional branches are
// checked one by one; object literals elaborate into union members; explicit call type
// arguments are checked against their constraints before the arguments.
type A = { a: number };
type B = { b: string; c: number };
type Offline = { data?: undefined; error: { code: string; issues: string[] } };
declare function getAny(): any;

export function r1(): number {
  return "x";
}
export async function r2(): Promise<number> {
  return "x";
}
export function r3(x: boolean): 3 {
  return (x ? (1) : 2);
}
export function r4(x: string): string {
  return x.startsWith("a") ? getAny() : 1;
}
export const r5 = (x: string): string => x.startsWith("a") ? getAny() : 1;
export async function r6<T>(call: Promise<T>): Promise<T | Offline> {
  try {
    return await call;
  } catch {
    return { error: { code: "NETWORK" } };
  }
}
export function u1(): string | A {
  return { a: "x" };
}
export function u2(): A | B {
  return { a: "x" };
}
export function u3<T>(): T | A {
  return { a: "x" };
}
export const u4: string | { e: { a: number; b: number } } = { e: { a: 1 } };
declare const s1: { a: string };
export function u5<T>(): T | A {
  return s1;
}

interface ObjA {
  y?: string;
}
interface MyObjA {
  x: string;
}
declare function g<P extends ObjA>(a: P): void;
g<MyObjA>({ x: "X" });
declare function h<P extends { y: string }>(a: P): void;
h<MyObjA>({ x: "X", z: 1 });
declare function k<P extends string>(a: P): void;
k<number>(1);
declare function k3<P extends { y: string }, Q extends { y: string }>(a: P, b: Q): void;
k3<MyObjA, MyObjA>({ x: "" }, { x: "" });
declare function o(a: number): void;
declare function o<P extends { y: string }>(a: P): void;
o<MyObjA>({ x: "" });
