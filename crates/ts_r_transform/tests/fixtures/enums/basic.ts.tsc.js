var E;
(function (E) {
    E[E["A"] = 0] = "A";
    E[E["B"] = 0] = "B";
    E[E["C"] = 1] = "C";
    E["D"] = "d";
    E["E"] = "e";
    E[E["F"] = -1] = "F";
    E[E["G"] = foo()] = "G";
    E[E["H"] = E.G + 1] = "H";
    E[E["I"] = 7] = "I";
    E[E["quoted key"] = 9] = "quoted key";
    E[E["J"] = -6] = "J";
    E[E["K"] = 8] = "K";
})(E || (E = {}));
export var E2;
(function (E2) {
    E2[E2["X"] = 10] = "X";
    E2[E2["Y"] = 11] = "Y";
    E2[E2["Z"] = 11] = "Z";
})(E2 || (E2 = {}));
(function (E) {
    E[E["L"] = 100] = "L";
})(E || (E = {}));
let a = 2 /* CE.Y */, b = "zz" /* CE["Z"] */, c = E.A;
function inner() { let Local; (function (Local) {
    Local[Local["A"] = 0] = "A";
})(Local || (Local = {})); return Local.A; }
