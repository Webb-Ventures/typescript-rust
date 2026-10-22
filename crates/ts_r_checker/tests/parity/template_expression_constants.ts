// A template expression of constants is that string literal (widened by `let`); a
// declared `const` without an initializer is not a constant (observed, tsc 7).
enum E { cat = "cat", n = 1 }
declare const lit: "a";
declare const s: string;
const x1 = `${E.cat}`;
const y1: "cat" = x1;
const x2 = `${lit}`;
const y2: "a" = x2;
const x3 = `p${E.n}`;
const y3: "p1" = x3;
const x4 = `${"q"}`;
const y4: "q" = x4;
const k = "k";
const x5 = `${k}`;
const y5: "k" = x5;
let x6 = `${E.cat}`;
const y6: "cat" = x6;
const x7 = `${s}`;
const y7: "z" = x7;
type Animal = { type: `${E.cat}`; meow: string } | { type: "dog"; bark: string };
function action(animal: Animal) {
  switch (animal.type) {
    case `${E.cat}`:
      return animal.meow;
    default:
      return animal.bark;
  }
}
