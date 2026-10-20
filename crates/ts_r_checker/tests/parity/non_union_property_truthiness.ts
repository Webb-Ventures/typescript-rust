// Truthiness or equality of a property narrows the object only when the property is a
// discriminant of a union (playwright's timeoutManager.ts and treeView.tsx, rxjs's
// group-by specs); the false branch of `instanceof` keeps interfaces structurally equal
// to the class (errors.ts).
declare function use(x: unknown): void;
const running = { timer: undefined, n: 1 };
if (running.timer)
  use(running.timer);
const ref = { current: null };
if (ref.current)
  use(ref.current);
if (1 > 0 && ref.current)
  use(ref.current);
class CustomError extends Error {}
class TargetClosedError extends CustomError {
  constructor(cause: string) { super(cause); }
}
function isTargetClosedError(error: Error) {
  return error instanceof TargetClosedError || error.name === 'TargetClosedError';
}
function f(error: Error | TypeError) {
  if (!(error instanceof TargetClosedError)) {
    const n: string = error.name;
  }
}
const config = { durationSkip: null, n: 1 };
const dur = config.durationSkip === null ? undefined : () => config.n;
if (typeof config.durationSkip !== 'object') use(config.n);
export { dur };
