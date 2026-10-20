// Constructor overloads with an implementation: a constructor returns no value, so no overload can be broken by its body's returns.
class Point {
  readonly x: number;
  constructor(x: number);
  constructor(x: string);
  constructor(x: number | string) {
    this.x = Number(x);
  }
}
export const x: number = new Point("1").x;
