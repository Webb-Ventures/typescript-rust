interface A {
  A: 1;
  [k: `x${string}`]: number;
  [k: Lowercase<string>]: number;
}
interface B {
  B: 2;
  [k: Lowercase<string>]: number;
}
declare const u: A | B;
u.x;
