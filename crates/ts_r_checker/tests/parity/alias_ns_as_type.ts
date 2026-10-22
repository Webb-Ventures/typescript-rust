// @filename: m.ts
export namespace N {
    export interface I {}
    export const v = 1;
}
export namespace T {
    export interface I {}
}
// @filename: a.ts
// An alias of a namespace with no type meaning, used as a type: TS2709.
import { N, T } from "./m";
namespace Q {
    export interface I {}
    class C {}
}
import q = Q;
import n2 = N;
let a: N;
let b: T;
let c: q;
let d: n2;
let e: q.I;
export {};
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "target": "ES2020", "module": "commonjs", "noEmit": true }, "files": ["m.ts", "a.ts"] }
