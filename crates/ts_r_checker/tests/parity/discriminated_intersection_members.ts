// A source with a union discriminant relates to a discriminated union whose
// members are intersections.
type Name = 'check' | 'click' | 'uncheck' | 'fill';
type Base = { name: Name; aria?: string };
type WithSel = Base & { selector: string; ref?: string };
type Click = WithSel & { name: 'click'; button: number };
type Check = WithSel & { name: 'check' };
type Uncheck = WithSel & { name: 'uncheck' };
type Fill = WithSel & { name: 'fill'; text: string };
type Action = Click | Check | Uncheck | Fill;
declare function rec(a: Action): void;
declare const c: boolean;
rec({ name: c ? 'check' : 'uncheck', selector: 's' });
rec({ name: c ? 'check' : 'uncheck', selector: 's', ref: undefined, aria: 'x' });
rec({ name: c ? 'check' : 'fill', selector: 's' });
type P1 = { kind: 'a'; v: number };
type P2 = { kind: 'b'; v: number };
const p: P1 | P2 = { kind: c ? 'a' : 'b', v: 1 };
