// @target: es2022
// TS1061: a member after a string member needs an initializer; tsc still emits it.
enum E { A = "x", B }
console.log(E.B);
