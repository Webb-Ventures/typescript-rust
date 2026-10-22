// @filename: a.d.ts
export as namespace Script;
// @filename: b.d.ts
export declare const v: number;
declare export as namespace Mod;
// @filename: c.ts
export const w = 1;
export as namespace Impl;
// @filename: d.d.ts
export namespace Inner {
    export as namespace Nested;
}
declare module "m" {
    export as namespace InMod;
}
// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2022", "module": "ESNext", "moduleResolution": "Bundler", "noEmit": true}, "files": ["a.d.ts", "b.d.ts", "c.ts", "d.d.ts"]}
