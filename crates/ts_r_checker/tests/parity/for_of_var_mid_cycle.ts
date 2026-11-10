// A `for`-`of` variable in the middle of a cycle closed on another variable through a
// checked node (`controlBlock`'s narrowing in the loop reads `case_`) is no cycle of
// variable types: TS7022 needs the whole path, recorded for each symbol on it (the React
// Compiler's ControlDominators.ts).
type Place = { id: number };
type Terminal =
  | { kind: 'if'; test: Place }
  | { kind: 'switch'; test: Place; cases: Array<{ test: Place | null }> };
declare const blocks: Map<number, { terminal: Terminal }>;

export function f(ids: Set<number>, isControl: (place: Place) => boolean): boolean {
  for (const blockId of ids) {
    const controlBlock = blocks.get(blockId)!;
    switch (controlBlock.terminal.kind) {
      case 'switch': {
        if (isControl(controlBlock.terminal.test)) {
          return true;
        }
        for (const case_ of controlBlock.terminal.cases) {
          if (case_.test !== null && isControl(case_.test)) {
            return true;
          }
        }
        break;
      }
    }
  }
  return false;
}

// A real cycle of variables is still reported, for each of them.

var n = o;
var o = { n };
