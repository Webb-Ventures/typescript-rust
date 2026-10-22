// `arguments` inside a function is the function's arguments object even when an outer
// scope declares a variable of that name.
var arguments = 10;
function foo(a: number) {
    arguments = 10;
    const z: string = arguments;
}
// @filename: tsconfig.json
{"compilerOptions": {"strict": false, "target": "ES2023", "lib": ["ES2023"], "noEmit": true}, "files": ["a.ts"]}
