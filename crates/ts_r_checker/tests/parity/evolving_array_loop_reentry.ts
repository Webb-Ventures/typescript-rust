// @filename: tsconfig.json
{"compilerOptions":{"strict":true,"target":"es2022","noEmit":true},"files":["a.ts"]}
// @filename: a.ts
declare function cond(): boolean;
function* g1() {
    var o = [];
    while (true) {
        o = yield* o;
    }
}
function* g2() {
    var o = [];
    for (let i = 0; i < 3; i++) {
        o = yield* o;
    }
    return o;
}
function f1() {
    let x = [];
    x = 5;
    const z: string = x;
}
function f2() {
    let a = [];
    for (const v of [1, 2]) {
        a = [...a, v];
    }
    const z: string = a;
}
function f3() {
    let a;
    a = [];
    while (cond()) {
        a = a.map(x => x);
    }
    const z: string = a;
}
