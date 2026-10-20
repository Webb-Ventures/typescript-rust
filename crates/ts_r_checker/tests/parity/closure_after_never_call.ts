// Code after a `never`-returning call is unreachable; a closure created there reads its
// parameters and outer constants with their declared types (not `never`), and a
// `@ts-expect-error` there is used by the error under it.
interface Item { kind: "a" | "b"; name: string }
declare const data: { items: Item[] };
declare function exit(code: number): never;
declare const proc: { exit(code: number): never };
declare function each(cb: (x: number) => void): void;

function viaFunction() {
  exit(0);
  each((x) => x.toFixed());
  const g = (x: number) => x.toFixed();
  const s: string = data.items;
}

proc.exit(0);
const picked = data.items.filter((item) => item.kind == "a");
picked.forEach((d) => {
  d.name;
  switch (d.kind) {
    case "a":
      break;
    // @ts-expect-error: not a kind
    case "c":
      break;
    default:
      throw new Error(d.kind);
  }
});
(() => data.items[0]?.name.length)();
