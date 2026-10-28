// `import * as y from "m"` of a module whose `export =` is only a type: TS2693, or TS2708
// for a namespace without values.
// @filename: tsconfig.json
{ "compilerOptions": { "strict": false, "noEmit": true, "target": "es2015", "module": "commonjs", "esModuleInterop": true }, "files": ["modules.d.ts", "main.ts"] }
// @filename: modules.d.ts
declare module "interface" {
    interface Foo { x: number; }
    export = Foo;
}
declare module "typens" {
    namespace Foo { interface X {} }
    export = Foo;
}
declare module "alias" {
    type Foo = { x: number };
    export = Foo;
}
// @filename: main.ts
import * as y1 from "interface";
import * as y2 from "typens";
import * as y3 from "alias";
y1.a;
y2;
y3;
let q: y1;
