// erasableSyntaxOnly: TS1294 for runtime-only TypeScript syntax outside ambient contexts.
// @filename: a.ts
namespace A.B.C { export const x = 1; }
namespace D { declare const y: number; }
namespace E { export import z = A.B; }
import type T = require("./b");
export declare namespace F { const q: number; }
declare global { enum G { a } }
declare module "m" { import a = require("./b"); export = a; }
namespace H { export namespace I { export type J = 1; } }
const enum K { a }
export import L = D;
class M { constructor(readonly p: number, private q = 1, @dec r?: number) {} }
declare function dec(...a: any[]): any;
namespace N { enum O {} }
namespace P { const enum Q {} }
namespace R { import S = A; }
class M2 { constructor(@dec public r: number) {} }
const w = <any>{};
function g() { return <any>
  1; }
// @filename: b.ts
export const v = 1;
// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ESNext", "module": "commonjs", "erasableSyntaxOnly": true, "experimentalDecorators": true, "noEmit": true}, "files": ["a.ts", "b.ts"]}
