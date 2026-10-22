interface Services { panic(message: string): never; }
function foo(services: Readonly<Services>, s: string | number): number {
    if (typeof s === "string") return 1;
    services.panic("ouch");
}
function bar(services: Readonly<Services>) {
    services.panic("x");
    let z = 1;
}
function baz(services: Readonly<{ log(message: string): void }>, s: string | number): number {
    if (typeof s === "string") return 1;
    services.log("ouch");
}
