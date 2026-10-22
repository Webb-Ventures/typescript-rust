// A context-sensitive object literal argument infers from its other properties before
// its functions are typed: `initialValues: 12` gives `V = number` (not the default
// `object`) for `nextValues: (val) => ..`, also through intersections and nested objects.
// With nothing to infer from, the default is still the context.
interface BaseProps<T> {
  initialValues: T;
  nextValues: (cur: T) => T;
}
declare function mk1<V = object>(p: BaseProps<V>): [V];
declare function mk2<P = {}, V = object>(p: P & BaseProps<V>): [P, V];
declare function mk3<V = object>(p: { inner: BaseProps<V> }): [V];
declare function mk4<V = object>(p: { f: (v: V) => void }): [V];
const s1: string = mk1({ initialValues: 12, nextValues: (val) => val + 1 });
const s2: string = mk2({ initialValues: 12, nextValues: (val) => val + 1 });
const s3: string = mk3({ inner: { nextValues: (val) => val + 1, initialValues: 1 } });
const s4: string = mk4({ f: (v) => v });
const s5: string = mk1({ initialValues: "a", nextValues: (val) => val.length });
