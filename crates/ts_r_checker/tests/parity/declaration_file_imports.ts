// Value imports of declaration files are TS2846 with the implementation file to use;
// type-only imports, side-effect imports and `import()` types are fine. Augmenting an
// untyped JavaScript module is TS2665 (not covered here: needs a package without types).
// @filename: x.d.ts
export type T = 1;
export declare const v: number;
// @filename: y.d.mts
export type T = 1;
// @filename: z.d.cts
export type T = 1;
// @filename: m.ts
import { T } from './x.d.ts';
import type { T as T2 } from './x.d.ts';
import { type T as T3 } from './x.d.ts';
import { T as T4 } from './y.d.mts';
import { T as T5 } from './z.d.cts';
export { T as T6 } from './x.d.ts';
export type { T as T7 } from './x.d.ts';
export * from './x.d.ts';
import * as ns from './x.d.ts';
import './x.d.ts';
type Q = import('./x.d.ts').T;
const dyn = import('./x.d.ts');
// @filename: n.d.ts
import { T } from './x.d.ts';
export declare const n: T;
