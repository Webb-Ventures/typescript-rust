// @filename: a.ts
// A local namespace before an exported function or class of its name in the same block.
namespace M {
    namespace F {
        var t;
    }
    export function F() { }
    namespace K {
        var t;
    }
    export class K { }
    namespace L {
        export type T = number;
    }
    export function L() { }
}
namespace M {
    namespace G {
        var t;
    }
}
namespace M {
    export function G() { }
}
// @filename: b.ts
namespace H {
    var t;
}
export function H() { }
// @filename: tsconfig.json
{ "compilerOptions": { "strict": false, "target": "ES2020", "module": "commonjs", "noEmit": true }, "files": ["a.ts", "b.ts"] }
