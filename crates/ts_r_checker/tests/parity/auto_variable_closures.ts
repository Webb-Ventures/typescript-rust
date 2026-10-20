// An auto-typed let assigned in a nested function: closures reading it get TS7005
// (TS7034 at the declaration); the declaring function sees only its own assignments.
declare function run(f: () => void): void;
declare function sub(cb: { error: (e: any) => void }): void;
declare function expectIt(x: unknown): void;

function a() {
  let err;
  sub({
    error: (e) => {
      err = e;
    },
  });
  expectIt(err);
  expectIt(err.message);
}

function b() {
  let recent;
  run(() => {
    recent = { url: "" };
  });
  run(() => {
    expectIt(recent);
  });
  expectIt(recent.url);
  run(() => recent.url);
}

function c() {
  let sink = null;
  run(() => {
    sink = { next(x: string) {} };
  });
  run(() => {
    sink?.next("x");
  });
  expectIt(sink);
}
