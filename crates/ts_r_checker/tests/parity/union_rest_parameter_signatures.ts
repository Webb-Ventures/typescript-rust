// A method of a union where one member takes a rest parameter (`Stack | Index[]`'s
// `push`): the combined signature intersects the rest parameter's element type with the
// other member's parameter at that position, and a type every member agrees on keeps its
// alias.
declare const TAG: unique symbol;
type Index = number & { [TAG]: "i" };
class Stack {
  push(n: Index) {}
}
declare const s: Stack | Index[];
declare const i: Index;
s.push(i);
s.push(1);
declare const t: { f(a: string, b: number): void } | { f(...xs: string[]): void };
t.f("a", 1);
t.f("a", "b");
declare const u: { f(...xs: string[]): void } | { f(a: string, b?: string): void };
u.f("a", "b");
u.f("a", 1);
declare const v: { f(a: string): void } | { f(...xs: string[]): void };
v.f("a", "b");
v.f();
declare const w: { f(a: string, ...r: number[]): void } | { f(...xs: string[]): void };
w.f("a", 1);
w.f("a");
