export {};
const enum Bits {
    A = 1,
    B = 2,
}
const x = Bits.A;
Bits.toString();
Bits.hasOwnProperty("A");
const y: string = Bits.constructor.name;
enum Plain {
    A,
}
Plain.toString();
