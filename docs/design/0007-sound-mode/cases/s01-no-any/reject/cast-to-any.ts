// Casting to any to reach a property (reported once, as S-01).
function count(value: object): number {
  return (value as any).count; // @expect TSR0101
}
export const c: number = count({});
