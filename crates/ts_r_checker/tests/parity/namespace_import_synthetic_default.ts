// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2023", "lib": ["ES2023"], "module": "commonjs", "noEmit": true}, "files": ["a.ts"]}
// @filename: b.d.ts
export function foo(): void;
export function bar(): void;
// @filename: fn.d.ts
declare function fn(): void;
declare namespace fn { const a: number; }
export = fn;
// @filename: cls.d.ts
declare class Cls { x: number; }
export = Cls;
// @filename: a.ts
import { default as B } from "./b";
import * as fns from "./fn";
import * as cls from "./cls";
B.bar();
fns.default();
fns.a;
fns();
cls.default;
cls.missing;
