// Module augmentations of re-exported names merge with the re-exported declaration
// (`export { Foo } from`, `export *`); conflicts are reported at a named re-export.
// @filename: b.ts
export interface Foo { a: number }
export class Bar { a = 1 }
export declare function Row(): void;
// @filename: a.ts
export { Foo } from "./b";
export type { Row } from "./b";
export * from "./b";
// @filename: c.ts
import "./a";
declare module "./a" {
  interface Foo { x: number }
  interface Bar { y: number }
  const Row: () => void;
}
import { Foo, Bar } from "./a";
declare const f: Foo; declare const b: Bar;
const n: string = f.x;
const m: string = b.y;
const k: string = f.a;
const k2: string = b.a;
