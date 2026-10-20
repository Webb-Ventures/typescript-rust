// A default import of a module without a default export names the module in types too.
// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2023", "lib": ["ES2023"], "esModuleInterop": true, "module": "ESNext", "moduleResolution": "Bundler", "noEmit": true}, "files": ["a.ts", "m.d.ts"]}
// @filename: m.d.ts
declare module "mymod" {
  class X { x: number }
  namespace inner { interface Y { y: string } }
  function make(): X;
}
// @filename: a.ts
import mm from "mymod";
export function f(x: mm.X, y: mm.inner.Y) {
  const n: number = x;
  const s: number = y;
  const v: number = mm.make;
}
