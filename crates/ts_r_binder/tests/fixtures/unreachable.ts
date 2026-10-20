// Unreachable code and unused labels (allowUnreachableCode/allowUnusedLabels: false).
function f(a: boolean) {
    return 1;
    let dead = 2;
}
function g() {
    while (true) { }
    g();
}
function h() {
    if (false) { h(); }
    unused: for (;;) { break; }
    throw new Error();
    var hoistedIsFine;
    const c = 1;
}
function i() {
    do { break; continue; } while (false);
    for (;;) {}
    return;
}
