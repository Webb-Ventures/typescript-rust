// An arrow function at the top level of a script captures the global `this` (TS7041 under
// noImplicitThis); `this` there without an arrow is the global object, quietly.
var f = () => { this.window; };
let f5 = () => () => this;
let f6 = function () { return () => this; };
var g = this;
// @filename: tsconfig.json
{
    "compilerOptions": {
        "strict": true, "target": "ES2023", "lib": ["ES2023", "DOM"], "module": "ESNext",
        "moduleResolution": "Bundler", "moduleDetection": "legacy", "noEmit": true
    }
}
