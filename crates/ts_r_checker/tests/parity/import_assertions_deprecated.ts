// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ESNext", "module": "esnext", "moduleResolution": "bundler", "resolveJsonModule": true, "noEmit": true}, "files": ["a.ts", "b.ts", "c.d.ts"]}
// @filename: data.json
{}
// @filename: types.d.ts
export interface MyType { x: string }
// @filename: a.ts
import json from "./data.json" assert { type: "json" };
import * as data from "./data.json" with { type: "json" };
export { default as config } from "./data.json" assert { type: "json" };
export { json, data };
// @filename: b.ts
type A = import("./types", { assert: { "resolution-mode": "import" } }).MyType;
type B = import("./types", { with: { "resolution-mode": "require" } }).MyType;
export type { A, B };
// @filename: c.d.ts
import json from "./data.json" assert { type: "json" };
type C = import("./types", { assert: { "resolution-mode": "import" } }).MyType;
export { json, C };
