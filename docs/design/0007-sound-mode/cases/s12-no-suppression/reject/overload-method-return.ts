// A method overload promises a string for numbers, but the body passes the number through.
class Labeler {
  label(value: string): string;
  label(value: number): string;
  label(value: string | number): string | number {
    if (typeof value === "string") return value.trim();
    return value; // @expect TSR1205
  }
}
export const l: string = new Labeler().label(1);
