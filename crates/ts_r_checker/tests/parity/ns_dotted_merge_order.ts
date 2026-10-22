// @filename: a.ts
// Each part of a dotted namespace name is checked against the class or function it
// merges with.
namespace A.B.C {
    export var p = 1;
}
namespace A {
    export namespace B {
        export class C {}
    }
}
namespace D.E {
    export var q = 1;
}
function D() {}
namespace F.G {
    export type T = number;
}
namespace F {
    export class G {}
}
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "target": "ES2020", "noEmit": true }, "files": ["a.ts"] }
