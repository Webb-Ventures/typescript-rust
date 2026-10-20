// @filename: tsconfig.json
{"compilerOptions":{"strict":true,"target":"es2022","noEmit":true},"files":["a.ts","b.ts"]}
// @filename: a.ts
declare class C {
    static X: number;
    static Alias: number;
    static method(): void;
    static get Accessor(): number;
    static Nested: number;
    static Value: number;
}
namespace C {
    export interface X { x: number }
    export type Alias = number;
    export interface method {}
    export interface Accessor {}
    export namespace Nested { export interface T {} }
    let x: X = { x: 1 };
    let a: Alias = 1;
    let m: method = {};
    let g: Accessor = {};
    let nested: Nested.T = {};
    Value;
    Nested;
}
// @filename: b.ts
namespace C {
    let x: X = { x: 1 };
    let a: Alias = 1;
    let m: method = {};
    let g: Accessor = {};
    let nested: Nested.T = {};
    Value;
    Nested;
}
