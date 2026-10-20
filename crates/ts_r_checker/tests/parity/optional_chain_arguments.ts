// The rest of an optional chain (arguments, element keys, closures in them) runs
// with each `?.` object not nullish.
declare const s: { frames: { id: number }[]; sel: number } | null;
const a = s?.frames.find(f => f.id === s.sel);
const b = s?.frames.find(f => s.sel === 1 ? f : null);
declare const st: { model: { s: { frames: { id: number }[]; sel: number } | null } };
const { s: s2 } = st.model;
const c = s2?.frames.find(f => f.id === s2.sel);
declare const conn: { check(o: {}): Promise<{ has: boolean }>; install(o: {}): Promise<void> } | undefined;
conn?.install({}).then(async () => {
  const { has } = await conn?.check({});
});
const d = s?.frames.map(x => s.sel + x.id);
let e2 = s;
const e = e2?.frames.find(f => f.id === e2.sel);
declare const s3: { frames: { id: number }[]; sel: number; f(n: number): void } | null;
s3?.f(s3.sel);
s3?.frames.indexOf(s3.frames[0]);
