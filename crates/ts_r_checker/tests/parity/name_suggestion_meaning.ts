// "Did you mean" suggestions (TS2552) only name symbols of the reference's meaning: a type
// is no suggestion for a value and a value none for a type; an export specifier takes any
// meaning. A namespace's exports are candidates inside it, `undefined` is a value candidate,
// and a module's own declarations come before globals (a script's are globals).
interface A { x: number }
type Alias = { y: string };
const alias = 1;
let r1 = a;
let r2 = Alia;
let r3: alia = 1;
namespace N {
    export class Identifier {}
    export namespace X { export const v = 1; }
    let r4 = Identifiers;
    let r5 = x;
}
class C7 extends Undefined {}
class ELoc {}
let r6 = loc;
type RoomInterfae = {};
export type { RoomInterface };
