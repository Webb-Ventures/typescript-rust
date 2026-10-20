// @target: es2022
declare const dec: any;
declare const registry: { tag(name: string): any; field: any };
const key = "computed";

@dec
export class Service {
    static instances = 0;
    @dec name = "svc";
    @registry.field count: number = 1;
    @dec(1) [key] = 2;
    @dec accessor enabled = true;
    @dec #secret = 3;
    @dec static create() { return new Service(); }
    @dec get label() { return this.name; }
    @dec set label(v: string) { this.name = v; }
    @dec #helper() { return this.#secret; }
    @dec accessor #flag = false;
    constructor(private readonly id: string) {
        Service.instances++;
    }
    run() { return this.#helper() + (this.#flag ? 1 : 0); }
}

export class Child extends Service {
    @registry.tag("child") method() {}
}

export default @dec class {}

const Named = @dec class {
    @dec static x = 1;
};

let assigned: any;
assigned = class { @dec y: any; };
