// A union of signatures where only some members have rest parameters folds pair by pair:
// the longer member's rest stays a rest, and a rest only on the shorter member becomes an
// extra rest after the longer member's parameters.
declare const arg3: ((a: { x: number }, ...objs: { y: number }[]) => number) | ((...objs: { x: number }[]) => number);
arg3({ x: 0 }, { x: 0, y: 0 }, { x: 0, y: 0 });
declare const arg6: ((a?: { x: number }, b?: { x: number }) => number) | ((...a: { y: number }[]) => number);
arg6({ x: 0, y: 0 }, { x: 0, y: 0 }, { y: 0 });
arg6({ x: 0, y: 0 }, { x: 0, y: 0 }, { x: 0 });
