// Instantiating T['k'] with an object type lacking 'k' is unknown, not any.
interface Opts { transformer?: string; errorFormatter?: (x: number) => void }
declare function create<TOptions extends Opts>(opts?: TOptions): { t: undefined extends TOptions['transformer'] ? false : true; u: TOptions['transformer'] };
const r = create({});
export const x: number = r;
const lit = {};
type K<T extends Opts> = T['transformer'];
export const y: [K<typeof lit>] = 1;
declare function c2<T extends Opts>(opts: T): T['transformer'];
export const z: number = c2({});
export const z2: number = c2(lit);
