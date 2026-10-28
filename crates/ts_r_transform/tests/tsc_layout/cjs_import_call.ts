// @module: commonjs
// @target: es2022
// Every non-literal `import()` reuses `s`; `import()` requires nothing; extra arguments drop.
declare var a: any;
import(a);
import(a);
import();
import("./0", {}, {});
