// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2022", "module": "commonjs", "noEmit": true}, "files": ["a1.ts", "a2.ts", "a3.ts", "a4.ts", "a5.ts", "a6.ts", "a7.ts", "a8.ts", "a9.ts", "a10.ts", "a11.d.ts", "a12.ts", "a13.ts", "a14.ts", "a15.ts"]}
// @filename: a1.ts
export interface I { }
var x = 1;
export = x;
// @filename: a2.ts
export type T = 1;
var x = 1;
export = x;
// @filename: a3.ts
export namespace N { }
var x = 1;
export = x;
// @filename: a4.ts
export namespace N { export var y = 1; }
var x = 1;
export = x;
// @filename: a5.ts
var x = 1;
export { };
export = x;
// @filename: a6.ts
var x = 1;
var y = 1;
export { y };
export = x;
// @filename: a7.ts
export * from "./a8";
var x = 1;
export = x;
// @filename: a8.ts
export const q = 1;
// @filename: a9.ts
var x = 1;
export = x;
export = x;
// @filename: a10.ts
var x = 1;
export declare var z: number;
export = x;
// @filename: a11.d.ts
declare var x: number;
export declare var z: number;
export = x;
// @filename: a12.ts
declare module "m" {
    var x: number;
    export var z: number;
    export = x;
}
// @filename: a13.ts
var x = 1;
export = x;
export default x;
// @filename: a14.ts
export type { T } from "./a2";
var x = 1;
export = x;
// @filename: a15.ts
export const enum CE { A }
export declare const enum DE { A }
var x = 1;
export = x;
