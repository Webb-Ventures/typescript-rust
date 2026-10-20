// JSON.stringify runs toJSON methods, so it may change box.value.
interface Box {
  value: string | null;
}
function measure(box: Box): number {
  if (box.value !== null) {
    const json = JSON.stringify({ toJSON(): number { box.value = null; return 0; } });
    return box.value.length + json.length; // @expect TSR0901
  }
  return 0;
}
export const m: number = measure({ value: "a" });
