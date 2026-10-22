// `module: es2015` has no dynamic imports: TS1323, and no TS1324 for the options.
// @filename: a.ts
export const foo = "./a.js";
// @filename: b.ts
export async function f() {
  const m = await import("./a.js");
  return import(m.foo, { with: { type: "json" } });
}
// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2020", "module": "ES2015", "moduleResolution": "bundler", "noEmit": true}, "files": ["a.ts", "b.ts"]}
