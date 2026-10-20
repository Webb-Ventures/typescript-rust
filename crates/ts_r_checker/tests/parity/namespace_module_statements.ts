// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2022", "module": "commonjs", "noEmit": true}, "files": ["a.ts", "b.d.ts", "c.ts"]}
// @filename: a.ts
export namespace M {
    export var v = 0;
    export { v as w };
    export * from "./c";
    export * as q from "./c";
    export { v as z } from "./c";
    import * as M2 from "./c";
    import M4 from "./c";
    import { x } from "./c";
    import "./c";
    import type { y } from "./c";
    export import M5 = require("./c");
    import M6 = require("nope");
    namespace Inner {
        import M7 = require("./c");
        export var u = 1;
        export { u as u2 };
    }
}
namespace A.B {
    import x = require("./nope");
    export * from "./nope";
    export { q } from "./nope2";
}
declare namespace D {
    import a = require("./c");
    export var v: number;
    export { v as w };
    export { x } from "./c";
}
export {};
// @filename: b.d.ts
declare module "amb" {
    namespace Q {
        import b = require("./c");
    }
}
declare namespace D2 {
    import a = require("./c");
    export var v: number;
    export { v as w };
}
// @filename: c.ts
export const x = 1;
export type y = 1;
export default 2;
