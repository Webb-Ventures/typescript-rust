// A TypeScript-private method can't be overridden either.
class Totals {
  readonly values: number[];
  readonly sum: number;
  constructor(values: number[]) {
    this.values = values;
    this.sum = 0;
    this.sum = this.compute();
  }
  private compute(): number {
    let s = 0;
    for (const v of this.values) s += v;
    return s;
  }
}
export const sum: number = new Totals([1, 2]).sum;
