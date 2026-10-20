// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "target": "ES2022", "module": "commonjs", "lib": ["ES2022"], "noEmit": true }, "files": ["v.ts", "c.ts", "e.ts", "fn.ts", "aug.ts", "use.ts"] }
// @filename: v.ts
// A module whose `export =` names an entity without namespace meaning (a variable, a
// class) cannot be augmented (TS2671) and its augmentation is dropped; an enum or a
// function merged with a namespace can be.
var x = 1;
export = x;
// @filename: c.ts
class C {}
export = C;
// @filename: e.ts
enum E { a }
export = E;
// @filename: fn.ts
function foo() {}
namespace foo { export var v = 1 }
export = foo;
// @filename: aug.ts
import v = require("./v");
import c = require("./c");
import e = require("./e");
import fn = require("./fn");
declare module "./v" { interface A { a: string } }
declare module "./c" { interface A { a: string } }
declare module "./e" { interface A { a: string } }
declare module "./fn" { interface A { a: string } }
// @filename: use.ts
import v = require("./v");
import fn = require("./fn");
import "./aug";
let a: v.A;
let b: fn.A = { a: 1 };
