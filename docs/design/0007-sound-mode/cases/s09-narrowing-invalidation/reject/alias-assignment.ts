// b may be the same object as a, so writing b.value drops the narrowing of a.value.
interface Slot {
  value: string | null;
}
function fill(a: Slot, b: Slot): number {
  if (a.value !== null) {
    b.value = null;
    return a.value.length; // @expect TSR0901
  }
  return 0;
}
const slot: Slot = { value: "a" };
export const n: number = fill(slot, slot);
