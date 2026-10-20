// A callback's contextual parameter types taken while a loop's flow is still being
// computed (the aliased condition `if (ok)` re-checks the call under loop assumptions)
// are not kept: the call's own check gives `x` its real type.
export function f(g: Set<string>, nodeTypes: Set<string>) {
  for (const nt of nodeTypes) {
    const ok = Array.from(g).some(x => nodeTypes.has(x));
    if (ok) {
      nt;
    }
  }
}

declare function report(a: string, b?: Set<string>): void;
export function h(orig: Map<string, { loc: number; nodeTypes: Set<string> }>, strict: Set<string>) {
  const gen = new Map<string, Set<string>>();
  for (const [key, { loc, nodeTypes }] of orig) {
    const g = gen.get(key);
    if (!g) {
      report(Array.from(nodeTypes).join(", "));
    } else {
      for (const nt of nodeTypes) {
        if (strict.has(nt) && !g.has(nt)) {
          const ok = Array.from(g).some(x => nodeTypes.has(x));
          if (ok) {
            report(nt);
          } else {
            report(nt, g);
          }
        }
      }
    }
    loc;
  }
}

export function k(g: Set<string>, all: Set<string>) {
  for (const nt of all) {
    const bad = Array.from(g).some(x => x.toFixed());
    if (bad) {
      nt;
    }
  }
}
