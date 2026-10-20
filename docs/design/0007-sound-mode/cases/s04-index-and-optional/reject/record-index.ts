// An index-signature value used without a check.
function score(scores: Readonly<Record<string, number>>, name: string): string {
  return scores[name].toFixed(1); // @expect TSR0401
}
export const s: string = score({ a: 1 }, "a");
