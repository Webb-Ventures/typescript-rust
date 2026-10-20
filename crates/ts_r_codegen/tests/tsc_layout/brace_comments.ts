if (x) { // c1
    x();
}
for (;;) { // c2
    x();
}
function f() { // c3
    x();
}
class C { // c4
    m() { // c5
        x();
    }
}
var o = { // c6
    a: 1
};
switch (x) { // c7
    case 1: // c8
        x();
}
{ // c9
    x();
}
var g = () => { // c11
    x();
};
function h() { /* c12 */ }
function k() {
    /* c13 */
}
if (x) { /* c14 */ }
if (x) {
    // c15
}
/* c16 */ x();
