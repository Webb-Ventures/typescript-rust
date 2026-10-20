// A field initialiser calls a method that reads a later field.
class Totals {
  size = this.compute(); // @expect TSR0802
  items: number[] = [1, 2];
  compute(): number {
    return this.items.length;
  }
}
export const size: number = new Totals().size;
