// @filename: importer.cts
// Under node20, `require` of an ES module is its `"module.exports"` export.
import Foo = require("./exporter.mjs");
new Foo();
import Foo2 from "./exporter.mjs";
new Foo2();
import * as Foo3 from "./exporter.mjs";
new Foo3();
import Bar = require("./typed.mjs");
new Bar();
// @filename: importer.cjs
const Bar = require("./typed.mjs");
new Bar();
// @filename: exporter.mts
export default class Foo {}
export { Foo as "module.exports" };
// @filename: typed.mts
export default class Bar {}
export type { Bar as "module.exports" };
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "target": "ES2023", "lib": ["ES2023"], "module": "node20", "allowJs": true, "checkJs": true, "noEmit": true }, "files": ["importer.cts", "importer.cjs", "exporter.mts", "typed.mts"] }
