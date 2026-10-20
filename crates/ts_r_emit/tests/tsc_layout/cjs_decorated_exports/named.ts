declare function dec(...a: any[]): any;
@dec
export class E {
    static make() { return new E(); }
}
@dec
class F {}
export { F };
