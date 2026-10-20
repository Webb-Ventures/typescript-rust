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
var __importDefault = (this && this.__importDefault) || function (mod) {
    return (mod && mod.__esModule) ? mod : { "default": mod };
};
Object.defineProperty(exports, "__esModule", { value: true });
exports.d3 = exports.d1 = exports.late = exports.NS = exports.En = exports.reexp = exports.alias = exports.local = exports.Cls = exports.mut = exports.L = exports.K = exports.z = exports.dd2 = exports.default = exports.yy = exports.x = exports.nsx = void 0;
exports.fn = fn;
exports.default = default_1;
// @target: es2022
// @module: commonjs
const fs_1 = __importDefault(require("fs"));
const path = __importStar(require("path"));
const d_1 = require("./d");
Object.defineProperty(exports, "reexp", { enumerable: true, get: function () { return d_1.a; } });
const e_1 = __importStar(require("./e"));
const f_1 = __importStar(require("./f")), ns2 = f_1;
require("./side");
__exportStar(require("./star"), exports);
exports.nsx = __importStar(require("./nsx"));
var x_1 = require("./x");
Object.defineProperty(exports, "x", { enumerable: true, get: function () { return x_1.x; } });
Object.defineProperty(exports, "yy", { enumerable: true, get: function () { return x_1.y; } });
var dd_1 = require("./dd");
Object.defineProperty(exports, "default", { enumerable: true, get: function () { return __importDefault(dd_1).default; } });
var dd2_1 = require("./dd2");
Object.defineProperty(exports, "dd2", { enumerable: true, get: function () { return __importDefault(dd2_1).default; } });
Object.defineProperty(exports, "z", { enumerable: true, get: function () { return dd2_1.z; } });
exports.K = 1, exports.L = 2;
exports.mut = 0;
function fn() { exports.mut++; return d_1.a + d_1.b + e_1.c + e_1.default + f_1.default + fs_1.default + path + ns2; }
class Cls {
}
exports.Cls = Cls;
function default_1() { }
const local = 1;
exports.local = local;
exports.alias = local;
exports.mut = 5;
[exports.mut] = [1];
({ mut: exports.mut } = { mut: 2 });
for (exports.mut of [1]) { }
(0, d_1.a)();
d_1.a.b();
(0, d_1.b) `t`;
var En;
(function (En) {
    En[En["A"] = 0] = "A";
})(En || (exports.En = En = {}));
var NS;
(function (NS) {
    NS.q = 1;
})(NS || (exports.NS = NS = {}));
let late = 1;
exports.late = late;
Promise.resolve().then(() => __importStar(require("./lazy")));
({ d1: exports.d1, d2: [exports.d3] } = { d1: 1, d2: [2] });
