if (x) { // c1
    x();
}
for (;;) { // c2
    x();
}
function f() {
    x();
}
class C {
    m() {
        x();
    }
}
var o = {
    a: 1
};
switch (x) { // c7
    case 1: // c8
        x();
}
{ // c9
    x();
}
var g = () => {
    x();
};
function h() { }
function k() {
    /* c13 */
}
if (x) { /* c14 */ }
if (x) {
    // c15
}
/* c16 */ x();
