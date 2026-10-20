function f() { return 1; }
function g() { }
function h() {
}
const a = () => { return 2; };
class C {
    m() { return 3; }
    get x() { return 4; }
    static { f(); }
}
class D {
}
if (a) {
    f();
}
else {
    g();
}
if (a)
    f();
else
    g();
if (a) { }
else { }
while (a)
    ;
for (;;) {
    break;
}
try {
    f();
}
catch (e) {
    g();
}
finally {
    h();
}
do
    f();
while (a);
do {
    f();
} while (a);
lbl: {
    break lbl;
}
{
    f();
}
switch (a) {
    case 1:
        f();
        break;
    case 2: f();
    case 3:
        g();
    case 4: {
        h();
    }
    default: break;
}
