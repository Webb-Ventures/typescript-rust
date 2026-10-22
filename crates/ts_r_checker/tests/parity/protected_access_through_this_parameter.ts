class Base {
    protected tag(): string { return ""; }
    protected static make(): void {}
}
class Left extends Base {
    protected left(): void {}
}
class Right extends Base {}
interface Mixed extends Left {}

function viaLeft<T extends Left>(this: T, other: Left, side: Right) {
    this.tag();
    other.left();
    side.tag();
}
function viaInterface(this: Mixed, other: Left) {
    other.left();
    Base.make();
}
function viaArrow(this: Left, other: Left) {
    const f = () => other.left();
    return f;
}
function unconstrained<T>(this: T, other: Left) {
    other.left();
}
function plain(other: Left) {
    other.tag();
}
export { viaLeft, viaInterface, viaArrow, unconstrained, plain };
