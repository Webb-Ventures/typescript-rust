// Method overloads with an implementation that satisfies each of them.
class Formatter {
  format(value: string): string;
  format(value: number): string;
  format(value: string | number): string {
    return String(value);
  }
}
export const f: string = new Formatter().format(1);
