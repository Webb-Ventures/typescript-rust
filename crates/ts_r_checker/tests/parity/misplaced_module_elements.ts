// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2022", "module": "commonjs", "noEmit": true, "allowJs": true, "checkJs": true}, "files": ["a.ts", "b.ts", "c.js"]}
// @filename: a.ts
function f() {
    namespace A { }
    declare namespace B { }
    module C { }
    declare module "x" { }
    declare global { }
    export = 1;
    export default 1;
    export { };
    export * from "./b";
    import x = A;
    import y = require("./b");
    import "./b";
    import z from "./nope";
    export var v = 1;
    export class K { }
    export default class K2 { }
    export declare var w: number;
    export interface II { }
    export type TT = 1;
    export enum EE { }
    export import xx = A;
    export namespace NN { }
    export function g() { }
    declare var q: number;
    declare class D { }
    if (q) export var z2 = 1;
}
namespace P {
    {
        namespace Q { }
    }
}
export {};
// @filename: b.ts
export const b = 1;
// @filename: c.js
function container() {
    import "fs";
    export { container };
}
