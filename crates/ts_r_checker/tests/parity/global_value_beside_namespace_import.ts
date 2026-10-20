// A `declare global` block that declares `var con` next to the file's `import * as con`:
// inside the block, `con.Con` (an interface's `extends`, a type alias) still names the
// import's namespace, because the merged global `con` is only a value.
con.warn("x");
alias.warn("y");
// @filename: con.d.ts
declare module "node:con" {
    namespace con {
        interface Con {
            warn(...data: any[]): void;
        }
    }
    var con: con.Con;
    export = con;
}
// @filename: glob.d.ts
export {};
import * as con from "node:con";
declare global {
    interface Con extends con.Con {}
    type ConAlias = con.Con;
    var con: Con;
    var alias: ConAlias;
}
