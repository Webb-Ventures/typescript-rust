// @filename: tsconfig.json
{"compilerOptions":{"strict":true,"target":"es2022","noEmit":true},"files":["a.ts"]}
// @filename: a.ts
declare function cond(): boolean;
function a1() {
    let x;
    x = [];
    const y: string = x;
    x.push(1);
    const z: string = x;
}
function a2() {
    let x;
    if (cond()) x = [];
    const y: string = x;
}
function a3() {
    let x = null;
    x = [];
    x.push("a");
    const y: number = x;
}
function a4() {
    let x;
    if (cond()) { x = []; x.push(1); } else { x = [true]; }
    const y: string = x;
    x.push(2);
}
function a5() {
    let x;
    for (let i = 0; i < 3; i++) { x = []; x.push(i); }
    const y: string = x;
}
function a6() {
    let x;
    (x = [], x).push(5);
    (x.push("hello"), x).push(true);
    ((x))[3] = { a: 1 };
    const r: string = x;
}
function a7() {
    let x = [];
    (x.push("hello"), x).push(true);
    const r: string = x;
}
