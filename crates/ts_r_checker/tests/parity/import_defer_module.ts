// `import defer` and `import.defer()` need `module` esnext or preserve (TS18060).
// @filename: a.ts
export function foo() {}
// @filename: b.ts
import defer * as ns from "./a.js";
ns.foo();
import.defer("./a.js").then((m) => m.foo());
// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2020", "module": "ES2020", "moduleResolution": "bundler", "noEmit": true}, "files": ["a.ts", "b.ts"]}
