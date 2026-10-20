// Names inside `declare module "m"` don't see the members of its `export =` namespace;
// `export =` / `export default` of a type or a namespace without values is no error.
// @filename: globals.d.ts
interface Ev {
    kind: "global";
}
declare module "lib" {
    type NativeEv = Ev;
    export = R;
    namespace R {
        interface Base<T = Element> {
            native: { kind: string };
            target: T;
        }
        interface Ev<T = Element> extends Base<T> {
            native: NativeEv;
        }
    }
    interface Element {
        tag: string;
    }
}
// @filename: ns.ts
namespace Types {
    export interface A {
        a: number;
    }
}
export = Types;
// @filename: iface.ts
interface Foo {
    foo: string;
}
export default Foo;
// @filename: reexport.ts
import type { A } from "./alias";
export default A;
// @filename: alias.ts
export interface A {
    a: number;
}
// @filename: use.ts
import Foo from "./iface";
import Types = require("./ns");
import Lib = require("lib");
const f: Foo = { foo: "x" };
const a: Types.A = { a: 1 };
declare const e: Lib.Ev;
const k: "global" = e.native.kind;
const bad: number = f.foo;
export { a, k, bad };
// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2023", "lib": ["ES2023"], "module": "preserve", "moduleResolution": "bundler", "noEmit": true}, "files": ["globals.d.ts", "ns.ts", "iface.ts", "reexport.ts", "alias.ts", "use.ts"]}
