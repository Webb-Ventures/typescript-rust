// @filename: reader.ts
export default class Reader {
    protected readonly _settings: number = 1;
    private x = 1;
    static make(): Reader { return new Reader(); }
}
// @filename: async.ts
import Reader from "./reader";
export default class AsyncReader extends Reader {
    protected readonly _settings: number = 2;
}
export class SyncReader extends Reader {
    protected readonly _settings: number = 3;
}
const r: Reader = Reader.make();
const bad: Reader = 1;
export { r, bad };
