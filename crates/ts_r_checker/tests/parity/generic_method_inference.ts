// Inferring between two generic method signatures: a union or intersection that mentions
// the source signature's own type parameter is not a candidate (es2025 `Set.union<U>`
// returns `Set<T | U>`), so `new Set(["a"])` still infers from `ReadonlySet<P>`
// (moderation policy.ts with the default lib).
type P = "a" | "b" | "c"
export const S: ReadonlySet<P> = new Set(["a", "b"])
export const M: ReadonlyMap<P, number> = new Map([["a", 1]])
interface RS3<T> { f<U>(o: U): T | U }
interface S3<T> { f<U>(o: U): T | U }
declare function g3<T>(s: S3<T>): T
declare const r3: RS3<"a" | "b">
const z3 = g3(r3)
const y3: boolean = z3
interface RS4<T> { f<U>(o: U): [T, U] }
interface S4<T> { f<U>(o: U): [T, U] }
declare function g4<T>(s: S4<T>): T
declare const r4: RS4<"a" | "b">
const z4: number = g4(r4)
// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "lib": ["ESNext"], "noEmit": true}, "files": ["a.ts"]}
