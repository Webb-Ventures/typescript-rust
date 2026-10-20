// An index-signature read checked before use.
function score(scores: Readonly<Record<string, number>>, name: string): string {
  const s = scores[name];
  return s === undefined ? "n/a" : s.toFixed(1);
}
export const s: string = score({ a: 1 }, "a");
