// @target: es2015
// @module: esnext
declare let it: any;
async function f1(c: any) {
    for await (const s of c) {}
}
async function f2(c: any) {
    for await (const s of it) {}
}
async function f3() {
    for (const s of it) { }
    for await (const s of it) {}
}
async function f4(a: any) {
    var a = 1;
    for (var i = 0; ;) { }
}
async function f5(a: any) {
    var x = 1;
    for (var i = 0; ;) { }
}
