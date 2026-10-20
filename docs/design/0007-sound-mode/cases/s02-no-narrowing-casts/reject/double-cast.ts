// Each assertion is checked: the inner one widens to unknown, the outer one narrows.
function toNumber(text: string): number {
  return (text as unknown) as number; // @expect TSR0201
}
export const n: number = toNumber("1");
