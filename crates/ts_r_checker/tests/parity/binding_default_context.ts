// trpc `stream/jsonl.ts` (`const { deserialize = (v) => v } = opts`): a default in a binding
// pattern is contextually typed by what the element receives.
declare const opts: { deserialize?: (v: string) => number; n?: number };
const { deserialize = (v) => v.length } = opts;
const { deserialize: d2 = (v) => {
  const q: null = v;
  return 1;
} } = opts;
function f(o: { cb?: (x: number) => void }) {
  const { cb = (x) => {} } = o;
  let { cb: c2 = (y) => {
    const q: null = y;
  } } = o;
}
const [g = (z) => z] = [] as ((z: boolean) => boolean)[];
function h({ k = (w) => w }: { k?: (w: string) => string }) {}
let r: (a: number) => void;
({ r = (a) => {} } = {} as { r?: (a: number) => void });
export {};
