// Script: a relative ambient module name is TS2436; `export` on an ambient module TS2668.
declare module "./rel" { }
declare module "../up" { }
declare module "/abs" { }
declare module "ok" { }
declare module "c:/drive" { }
namespace N {
    export declare module "M" { }
}
// @filename: b.ts
export declare module "m2" { }
export declare global { }
export { };
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "moduleResolution": "Bundler", "moduleDetection": "auto", "noEmit": true }, "files": ["a.ts", "b.ts"] }
