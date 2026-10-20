// An exported function that declares what it throws.
/** @throws {RangeError} when the text is not a whole number */
export function port(text: string): number {
  const n = Number(text);
  if (!Number.isInteger(n)) throw new RangeError("not a port");
  return n;
}
