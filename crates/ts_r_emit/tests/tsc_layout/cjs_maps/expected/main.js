"use strict";
var __createBinding = (this && this.__createBinding) || (Object.create ? (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    var desc = Object.getOwnPropertyDescriptor(m, k);
    if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
      desc = { enumerable: true, get: function() { return m[k]; } };
    }
    Object.defineProperty(o, k2, desc);
}) : (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    o[k2] = m[k];
}));
var __setModuleDefault = (this && this.__setModuleDefault) || (Object.create ? (function(o, v) {
    Object.defineProperty(o, "default", { enumerable: true, value: v });
}) : function(o, v) {
    o["default"] = v;
});
var __importStar = (this && this.__importStar) || (function () {
    var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function (o) {
            var ar = [];
            for (var k in o) if (Object.prototype.hasOwnProperty.call(o, k)) ar[ar.length] = k;
            return ar;
        };
        return ownKeys(o);
    };
    return function (mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        __setModuleDefault(result, mod);
        return result;
    };
})();
var __exportStar = (this && this.__exportStar) || function(m, exports) {
    for (var p in m) if (p !== "default" && !Object.prototype.hasOwnProperty.call(exports, p)) __createBinding(exports, m, p);
};
Object.defineProperty(exports, "__esModule", { value: true });
exports.keys = exports.second = exports.first = exports.E = exports.Plain = exports.Greeter = exports.count = exports.Stub = exports.dep = void 0;
exports.bump = bump;
exports.ids = ids;
const dep = __importStar(require("./dep"));
exports.dep = dep;
const dep_1 = require("./dep");
__exportStar(require("./dep"), exports);
exports.Stub = null;
exports.count = (0, dep_1.helper)(2);
function bump() {
    exports.count++;
    return exports.Stub;
}
function* ids() {
    yield exports.count;
}
class Greeter {
    constructor(name, ...rest) {
        this.name = name;
        this.greeting = "hi";
    }
}
exports.Greeter = Greeter;
class Plain {
    constructor() {
        this.x = 10;
    }
}
exports.Plain = Plain;
var A;
(function (A) {
    var B;
    (function (B) {
        B.x = 1;
        function f() {
            return B.x;
        }
        B.f = f;
    })(B = A.B || (A.B = {}));
})(A || (A = {}));
var E;
(function (E) {
    E[E["a"] = 0] = "a";
    E[E["b"] = 3] = "b";
    E[E["c"] = 4] = "c";
})(E || (exports.E = E = {}));
({ first: exports.first, second: exports.second = 2 } = { first: 1 });
exports.keys = ["a", "b"].filter((k) => k);
//# sourceMappingURL=main.js.map