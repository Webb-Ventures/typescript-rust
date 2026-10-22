// @filename: types.ts
// Under isolatedModules, re-exports of types must be type-only unless the import is.
export interface I { x: number }
export class C {}
export type { C as TC };
export declare const enum E { A }
// @filename: iso.ts
import { I, TC, E } from "./types";
export { I, TC };
import type { I as J } from "./types";
export { J };
import { C } from "./types";
const C = 1;
export const y: I = { x: C + E.A };
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "target": "ES2020", "module": "esnext", "moduleResolution": "bundler", "isolatedModules": true, "noEmit": true }, "files": ["types.ts", "iso.ts"] }
