// An exhaustive switch's implicit default adds nothing where it joins other paths;
// as the only path it passes types through. A non-unit case (`case a === b:`
// in `switch (true)`) makes the switch non-exhaustive.
export type LocatorBase = 'page' | 'locator' | 'frame-locator';
type Kind = 'default' | 'other';
export class F {
  gen(base: LocatorBase, kind: Kind, body: string, options: { hasText?: string } = {}): string {
    let clazz: string;
    switch (base) {
      case 'page': clazz = 'Page'; break;
      case 'frame-locator': clazz = 'FrameLocator'; break;
      case 'locator': clazz = 'Locator'; break;
    }
    switch (kind) {
      case 'default':
        if (options.hasText !== undefined)
          return `${clazz}`;
        return body;
      case 'other':
        return clazz;
    }
  }
}
export function g(base: LocatorBase): string {
  let clazz: string;
  switch (base) {
    case 'page': clazz = 'Page'; break;
    case 'frame-locator': clazz = 'FrameLocator'; break;
    case 'locator': clazz = 'Locator'; break;
  }
  return clazz;
}
export function h(a: number, b: number): string {
  let r: string;
  switch (true) {
    case a === b: r = "eq"; break;
    case a !== b: r = "ne"; break;
  }
  return r;
}
function p1(x: 'a' | 'b') { switch (x) { case 'a': return 1; case 'b': return 2; } x.foo; }
function p2(x: 'a' | 'b', y: string) { switch (x) { case 'a': return 1; case 'b': return 2; } y.foo; new TypeError(Object.constructor.name); }
function p3(o: { k: 'a' | 'b' }) { switch (o.k) { case 'a': return 1; case 'b': return 2; } o.k.foo; o.foo; }
function p4(x: 'a' | 'b', y: string | number) { if (typeof y === 'string') { switch (x) { case 'a': return 1; case 'b': return 2; } y.foo; } }
