// `import defer` needs --module esnext or preserve (TS18060).
// @filename: a.ts
export function foo() { }
// @filename: b.ts
import defer * as ns from "./a.js";
ns.foo();
import.defer("./a.js").then(m => m.foo());
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "target": "ES2022", "module": "es2022", "moduleResolution": "bundler", "noEmit": true } }
