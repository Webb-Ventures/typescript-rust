// @filename: b.d.ts
export declare const v: number;
export interface T { n: number }
export as namespace Mod;
// @filename: e.ts
export const z = Mod.v;
export const t: Mod.T = { n: typeof Mod === "object" ? 1 : 2 };
// @filename: script.ts
const s = Mod.v;
// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2022", "module": "ESNext", "moduleResolution": "Bundler", "noEmit": true}, "files": ["b.d.ts", "e.ts", "script.ts"]}
