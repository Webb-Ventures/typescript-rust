// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ESNext", "module": "esnext", "moduleResolution": "bundler", "noEmit": true}, "files": ["b.ts"]}
// @filename: types.d.ts
export interface MyType { x: string }
// @filename: b.ts
const a = import("./types", { assert: { "resolution-mode": "import" } });
const b = import("./types", { "assert": { "resolution-mode": "import" } });
const c = import("./types", { with: { "resolution-mode": "import" } });
const d = import("./types", { with: {}, assert: {} });
const opts = { assert: {} };
const e = import("./types", opts);
export { a, b, c, d, e };
