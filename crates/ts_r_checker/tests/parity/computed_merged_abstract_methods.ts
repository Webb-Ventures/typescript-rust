const method: unique symbol = Symbol();
abstract class F { abstract [method](): void; }
interface F { [method](): void; }
const name = "named";
abstract class G { abstract [name](): void; }
interface G { [name](): void; }
export {};
