// One generic signature instead of overloads.
function pad<T extends string | number>(value: T): string {
  return String(value).padStart(4, "0");
}
export const p: string = pad(7) + pad("8");
