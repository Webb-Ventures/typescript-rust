// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "moduleResolution": "Bundler", "esModuleInterop": true, "noEmit": true }, "files": ["a.ts", "m.ts", "d.d.ts", "e.d.ts", "g.d.ts"] }
// @filename: a.ts
// trpc `import http from "http"`: under esModuleInterop a default import of a declaration
// file or ambient module without a default export is the module itself; a TypeScript
// source module, an ESM-format declaration file (typeorm `import chai from "chai"`) or one
// exporting `__esModule` still needs a default export (TS1192).
import m from "./m";
import d from "./d";
import e from "./e";
import amb from "amb";
import amb2 from "amb2";
import esm from "esm-pkg";
import cjs from "cjs-pkg";
const a1: null = m;
const a2: null = d;
const a3: null = e;
const a4: null = amb;
const a5: null = amb2;
const a6: null = esm;
const a7: null = cjs;
amb.z.length;
amb.make((r) => {
  const q: null = r;
});
// @filename: m.ts
export const x = 1;
// @filename: d.d.ts
export declare const y: number;
// @filename: e.d.ts
export declare const y: number;
export declare const __esModule: true;
// @filename: g.d.ts
declare module "amb" {
  export const z: string;
  export function make(cb: (r: { id: number }) => void): void;
}
declare module "amb2" {
  export const z: string;
  export const __esModule: true;
}
// @filename: node_modules/esm-pkg/package.json
{ "name": "esm-pkg", "type": "module", "types": "./index.d.ts" }
// @filename: node_modules/esm-pkg/index.d.ts
export declare const v: number;
// @filename: node_modules/cjs-pkg/package.json
{ "name": "cjs-pkg", "types": "./index.d.ts" }
// @filename: node_modules/cjs-pkg/index.d.ts
export declare const v: number;
