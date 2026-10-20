// Array.isArray narrows to any[] in lib; sound mode sees unknown[], so the element is unknown.
function first(input: unknown): string {
  if (Array.isArray(input)) {
    const s: string = input[0]; // @expect TSR0102
    return s;
  }
  return "";
}
export const f: string = first(["a"]);
