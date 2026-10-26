// TS2714: an `export =` or `export default` in an ambient context must be a dotted name.
/// <reference path="./foo.d.ts" />
/// <reference path="./mods.d.ts" />
import "indirect";
import "ok";
// @filename: foo.d.ts
export default 2 + 2;
export as namespace Foo;
// @filename: mods.d.ts
declare module "indirect" {
    export default typeof Foo.default;
}
declare module "ok" {
    const x: number;
    export = x;
}
declare module "paren" {
    const y: number;
    export default (y);
}
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "target": "ES2023", "lib": ["ES2023"], "module": "commonjs", "noEmit": true }, "files": ["a.ts"] }
