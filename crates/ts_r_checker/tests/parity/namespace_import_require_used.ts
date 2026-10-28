// `import x = require("m")` in a namespace is TS1147; when `x` is used its module is still
// looked up (TS2307). A script's `declare module "m"` nested in a namespace (TS2435) is a
// member of the namespace, not an ambient module.
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "target": "es2015", "module": "commonjs", "noEmit": true }, "files": ["a.ts", "b.ts"] }
// @filename: a.ts
export namespace used {
    import foo = require("not_there_1");
    export var a = foo.x;
}
export namespace unused {
    import bar = require("not_there_2");
}
// @filename: b.ts
namespace outer {
    declare module "nested_mod" {
        export var y: number;
    }
    import m = require("nested_mod");
    export var b = m.y;
}
