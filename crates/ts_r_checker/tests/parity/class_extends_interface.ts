interface I {}
interface IG<T> {}
type TA = {};
namespace N { export interface NI {} }
class S1 extends I {}
class S2 extends TA {}
class S3 extends IG<string> {}
class S4 extends N.NI {}
const e = class extends I {};
class S5 extends Missing {}
export {};
