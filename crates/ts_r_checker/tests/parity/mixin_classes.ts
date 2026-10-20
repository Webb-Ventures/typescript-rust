// Mixin classes: `class extends Base` inside a generic function makes `typeof C & T`;
// `new` on an intersection with a mixin constructor intersects instance types in member order.
type Constructor<T = {}> = new (...args: any[]) => T;

function Tagged<T extends Constructor>(Base: T) {
    return class extends Base {
        tag = "";
        static count = 0;
    };
}

class Point {
    constructor(public x: number, public y: number) {}
    protected secret = 1;
    static zero = 0;
}

const TaggedPoint = Tagged(Point);
const tp = new TaggedPoint(1, 2);
const x: number = tp.x;
const tag: string = tp.tag;
const zero: number = TaggedPoint.zero;
const count: number = TaggedPoint.count;
const bad: string = tp.x;

class Sub extends TaggedPoint {
    read() {
        return this.secret + this.x;
    }
}
const s = new Sub(1, 2);
const t: string = s.tag;
s.secret;

declare const Z: typeof Point & (new (...args: any[]) => { z: 1 });
const z = new Z(1, 2);
const zz: 1 = z.z;
const zx: number = z.x;
const zbad: 2 = z.z;
