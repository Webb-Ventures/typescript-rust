// // A mutable variable whose literal-typed initializer is not fresh widens to the literal's base
// // type when it is assigned compound-like: x op= y, or x = a op b with an arithmetic, shift or
// // bitwise operator. Plain assignments and method calls keep the literal type.
export {};
const empty: "" = "";
let a1 = empty;
a1 = "x";
let a2 = empty;
a2 = a2 + "bar";
let a3 = empty;
a3 = "y" + a3;
let a4 = empty;
a4 = (a4 + "z");
let a5 = empty;
a5 += "w";
let a6 = empty;
const q: number = a6;
a6 = a6 + "1";
let a7 = empty;
a7 = a7.toUpperCase();
