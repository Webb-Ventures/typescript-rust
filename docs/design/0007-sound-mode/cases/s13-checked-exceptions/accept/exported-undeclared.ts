// An exported function that throws without a @throws tag: fine by default, TSR1302 with exports-declare-throws.
export function port(text: string): number { // @opt-in TSR1302
  const n = Number(text);
  if (!Number.isInteger(n)) throw new RangeError("not a port");
  return n;
}
