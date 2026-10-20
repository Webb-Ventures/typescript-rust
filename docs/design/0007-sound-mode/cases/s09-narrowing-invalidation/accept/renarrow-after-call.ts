// Checking again after the call.
interface Holder {
  value: string | null;
}
function read(o: Holder, tick: () => void): number {
  tick();
  if (o.value !== null) return o.value.length;
  return 0;
}
export const n: number = read({ value: "a" }, () => undefined);
