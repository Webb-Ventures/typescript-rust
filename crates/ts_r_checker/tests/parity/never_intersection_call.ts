// An intersection whose property is literal in one member and has no common value with
// the other's type is `never`, which has no call signatures.
declare const f: { (x: string): number; a: "" } & { a: number }
f()
declare const g: { (x: string): number; a: string } & { a: number }
g("x")
