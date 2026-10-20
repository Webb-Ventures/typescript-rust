// typeorm `AbstractSqliteQueryRunner.ts` (`while ((fkResult = fkRegex.exec(sql)) !== null)`):
// an assignment (or the last expression of a comma list) in a condition tests its target.
declare function ex(): RegExpExecArray | null;
declare function ex2(): string[] | null;
function f(sql: string) {
  let r: RegExpExecArray | null;
  if ((r = ex()) !== null) {
    const a = r[1];
  }
  let r2: string[] | null;
  if ((r2 = ex2()) != undefined) {
    const a = r2[1];
  } else {
    const n: undefined = r2;
  }
  const re = /a/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(sql)) !== null) {
    const a: string = m[0];
  }
  let s: string | number | undefined;
  if (typeof (s = Math.random() ? "a" : 1) === "string") {
    const n: null = s;
  }
  let t: string | undefined;
  if ((ex(), t = ex()?.[0]) !== undefined) {
    const n: null = t;
  }
  let q: string | undefined;
  if ((q ??= ex()?.[0])) {
    const n: null = q;
  }
}
export {};
