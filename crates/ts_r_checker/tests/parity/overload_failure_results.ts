// A failed call of overloaded non-generic signatures has the intersection of their return
// types; a union property with a setter is written as the union of its members' write types.
declare function two(a: number, b: number): number;
declare function two(a: string): string;
declare function three(a: number, b: string): number;
declare function three(a: string, b: number): string;
const r1 = two(true); r1.foo;
const r2 = two(); r2.foo;
const r3 = three(true, true); r3.foo;
const r4 = two(1, 2, 3); r4.foo;
declare function same(a: number): number[];
declare function same(a: string): number[];
const r5 = same(true); r5.foo;
export {};
let s4: string = three(true, true);
let s6: boolean = two(true);
declare function one(a: number): number;
let s1: string = one();
class One { get p(): string { return ""; } set p(v: string | number) {} q = 1; }
class Two { get p(): string { return ""; } set p(v: string) {} get q(): string { return ""; } set q(v: string | boolean) {} }
declare const u: One | Two;
u.p = 42;
u.q = true;
u.p = false;
