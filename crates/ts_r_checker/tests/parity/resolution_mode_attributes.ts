// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2022", "module": "nodenext", "noEmit": true}, "files": ["a.ts", "b.ts", "c.mts", "d.cts"]}
// @filename: node_modules/pkg/package.json
{"name": "pkg", "version": "1.0.0", "exports": {"import": "./i.js", "require": "./r.js"}}
// @filename: node_modules/pkg/i.d.ts
export interface I {}
// @filename: node_modules/pkg/r.d.ts
export interface R {}
// @filename: a.ts
export {};
import type { R } from "pkg" with { "resolution-mode": "foo" };
import type { I } from "pkg" with { "resolution-mode": "import" };
import { type R as R2 } from "pkg" with { "resolution-mode": "require" };
import { type R as R3 } from "pkg" with { "resolution-mode": "bar" };
export type { R as R4 } from "pkg" with { "resolution-mode": "baz" };
export * from "pkg" with { type: "json" };
export type T = import("pkg", { with: { "resolution-mode": "qux" } }).R;
export type U = R | I | R2 | R3;
// @filename: b.ts
import type { R } from "pkg" with { "resolution-mode": "require", type: "json" };
export type { R };
// @filename: c.mts
import type { R } from "pkg" with { "resolution-mode": "foo" };
import { type I } from "pkg" with { "resolution-mode": "import" };
export type U = R | I;
// @filename: d.cts
import { type R } from "pkg" with { "resolution-mode": "require" };
export type { I } from "pkg" with { "resolution-mode": "import" };
export type U = R;
