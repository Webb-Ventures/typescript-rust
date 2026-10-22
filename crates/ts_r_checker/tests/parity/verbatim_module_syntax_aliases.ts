// @filename: types.ts
// Under verbatimModuleSyntax, imports and re-exports of types must be type-only.
export interface I { x: number }
export class C {}
export type { C as TC };
export declare const enum E { A }
// @filename: main.ts
import { I, TC } from "./types";
import { E } from "./types";
export { I };
export { TC as TC2 };
import type { I as J } from "./types";
export { J };
export const x: I | J = { x: E.A };
new TC();
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "target": "ES2020", "module": "esnext", "moduleResolution": "bundler", "verbatimModuleSyntax": true, "noEmit": true }, "files": ["types.ts", "main.ts"] }
