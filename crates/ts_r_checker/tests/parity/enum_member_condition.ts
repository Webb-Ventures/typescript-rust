enum Nums { Zero = 0, One = 1 }
enum Strs { Empty = "", A = "A" }
declare const b: boolean;
const a1 = Nums.Zero ? "a" : "b";
if (Strs.A) {}
if (b && Nums.One) {}
if (Nums.Zero || b) {}
if ((b || Strs.Empty) && b) {}
while (Nums.One) { break; }
const v = Nums.One && 1;
const w = b || Nums.Zero;
import Alias = Nums;
if (Alias.One) {}
namespace NS { export enum E { X, Y } }
if (NS.E.Y) {}
declare const n: Nums;
if (n) {}
const enum C { K = 1 }
if (C.K) {}
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "moduleResolution": "Bundler", "noEmit": true }, "files": ["a.ts"] }
