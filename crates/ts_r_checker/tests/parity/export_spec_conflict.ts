// @filename: a.ts
const y = 0;
export let x = 1;
const x2 = 2;
export { x2 as x };
export { y };
// @filename: b.ts
export namespace M {
    export let x = 1;
}
const x = 0;
export namespace M {
    export { x };
}
// @filename: c.ts
interface I {}
export interface J {}
export { I as J };
type T = number;
export var T2 = 1;
export { T as T2 };
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "target": "ES2020", "module": "commonjs", "noEmit": true }, "files": ["a.ts", "b.ts", "c.ts"] }
