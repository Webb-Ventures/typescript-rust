// Block-scoped names read before their declaration: an async or generator function
// called where it is written runs now for later declarations (not for its own
// variable), and a parameter decorator runs with its class.
function f() {
    const promise = (async () => {
        promise;
        foo;
        await null;
        promise;
        foo;
    })();
    const gen = (function* () { gen; bar; })();
    const foo = 1;
    const bar = 1;
}
function deco(...args: any[]): any {}
class C {
    greet2(@lambda(Enum.No) @deco param: any) {}
    m(@deco p: any) { lambda; }
}
const lambda = (...args: any[]): any => {};
enum Enum { No = 0 }
export {};
// @filename: tsconfig.json
{
    "compilerOptions": {
        "strict": true, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext",
        "moduleResolution": "Bundler", "moduleDetection": "force", "noEmit": true,
        "experimentalDecorators": true
    },
    "files": ["a.ts"]
}
