// A variable without a type or initializer is only auto-typed under noImplicitAny and when
// not ambient, so with noImplicitAny off a call of it with type arguments is TS2347.
declare var a;
a<number>();
var nake;
function f() {
  nake<number>();
}
// @filename: tsconfig.json
{
    "compilerOptions": {
        "strict": false, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext",
        "moduleResolution": "Bundler", "moduleDetection": "force", "noEmit": true
    }
}
