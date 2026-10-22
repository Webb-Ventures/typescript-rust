// @filename: types.ts
// Under isolatedModules, `export default T` of an imported type must be marked type-only.
export interface Date { day: number }
export type T = number;
export class C {}
export type { C as TC };
// @filename: a.ts
import { T } from "./types";
export default T;
// @filename: b.ts
import { TC } from "./types";
export default TC;
// @filename: c.ts
import type { T } from "./types";
export default T;
// @filename: shadow.ts
import { Date } from "./types";
export const now = new Date();
export const d: Date = { day: now.getDay() };
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "target": "ES2020", "module": "esnext", "moduleResolution": "bundler", "isolatedModules": true, "noEmit": true }, "files": ["types.ts", "a.ts", "b.ts", "c.ts", "shadow.ts"] }
