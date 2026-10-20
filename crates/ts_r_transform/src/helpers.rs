//! Runtime helpers, printed verbatim after the directive prologue (tsc's emit helpers).

use crate::options::TransformOptions;

#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord, Hash)]
pub(crate) enum Helper {
    CreateBinding,
    SetModuleDefault,
    ImportStar,
    ExportStar,
    ImportDefault,
    Decorate,
    Metadata,
    Param,
    RunInitializers,
    EsDecorate,
    SetFunctionName,
    PropKey,
    RewriteRelativeImportExtension,
    Awaiter,
    Rest,
    AddDisposableResource,
    DisposeResources,
    Await,
    AsyncGenerator,
    AsyncValues,
    AsyncDelegator,
    ClassPrivateFieldGet,
    ClassPrivateFieldSet,
    ClassPrivateFieldIn,
    MakeTemplateObject,
}

impl Helper {
    /// Helpers printed in this order, before the [`Helper::REQUEST_ORDER`] ones.
    pub const ALL: [Helper; 15] = [
        Helper::CreateBinding,
        Helper::SetModuleDefault,
        Helper::ImportStar,
        Helper::ExportStar,
        Helper::ImportDefault,
        Helper::Decorate,
        Helper::Metadata,
        Helper::Param,
        Helper::Awaiter,
        Helper::Rest,
        Helper::RunInitializers,
        Helper::EsDecorate,
        Helper::PropKey,
        Helper::SetFunctionName,
        Helper::RewriteRelativeImportExtension,
    ];

    /// Helpers printed after every [`Helper::ALL`] one, in the order the file first asks for
    /// them (observed: `__await, __asyncGenerator, __asyncValues, __asyncDelegator` follow
    /// `__awaiter`, `__rest` and `__setFunctionName` whatever the source order;
    /// `__addDisposableResource, __disposeResources` follow `__awaiter` and precede
    /// `__await`, as `using` is lowered before async functions).
    pub const REQUEST_ORDER: [Helper; 10] = [
        Helper::ClassPrivateFieldGet,
        Helper::ClassPrivateFieldSet,
        Helper::ClassPrivateFieldIn,
        Helper::AddDisposableResource,
        Helper::DisposeResources,
        Helper::Await,
        Helper::AsyncGenerator,
        Helper::AsyncValues,
        Helper::AsyncDelegator,
        Helper::MakeTemplateObject,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Helper::CreateBinding => "__createBinding",
            Helper::SetModuleDefault => "__setModuleDefault",
            Helper::ImportStar => "__importStar",
            Helper::ExportStar => "__exportStar",
            Helper::ImportDefault => "__importDefault",
            Helper::Decorate => "__decorate",
            Helper::Metadata => "__metadata",
            Helper::Param => "__param",
            Helper::RunInitializers => "__runInitializers",
            Helper::EsDecorate => "__esDecorate",
            Helper::SetFunctionName => "__setFunctionName",
            Helper::PropKey => "__propKey",
            Helper::RewriteRelativeImportExtension => "__rewriteRelativeImportExtension",
            Helper::Awaiter => "__awaiter",
            Helper::Rest => "__rest",
            Helper::AddDisposableResource => "__addDisposableResource",
            Helper::DisposeResources => "__disposeResources",
            Helper::Await => "__await",
            Helper::AsyncGenerator => "__asyncGenerator",
            Helper::AsyncValues => "__asyncValues",
            Helper::AsyncDelegator => "__asyncDelegator",
            Helper::ClassPrivateFieldGet => "__classPrivateFieldGet",
            Helper::ClassPrivateFieldSet => "__classPrivateFieldSet",
            Helper::ClassPrivateFieldIn => "__classPrivateFieldIn",
            Helper::MakeTemplateObject => "__makeTemplateObject",
        }
    }

    fn text(self) -> &'static str {
        match self {
            Helper::AddDisposableResource => {
                r#"var __addDisposableResource = (this && this.__addDisposableResource) || function (env, value, async) {
    if (value !== null && value !== void 0) {
        if (typeof value !== "object" && typeof value !== "function") throw new TypeError("Object expected.");
        var dispose, inner;
        if (async) {
            if (!Symbol.asyncDispose) throw new TypeError("Symbol.asyncDispose is not defined.");
            dispose = value[Symbol.asyncDispose];
        }
        if (dispose === void 0) {
            if (!Symbol.dispose) throw new TypeError("Symbol.dispose is not defined.");
            dispose = value[Symbol.dispose];
            if (async) inner = dispose;
        }
        if (typeof dispose !== "function") throw new TypeError("Object not disposable.");
        if (inner) dispose = function() { try { inner.call(this); } catch (e) { return Promise.reject(e); } };
        env.stack.push({ value: value, dispose: dispose, async: async });
    }
    else if (async) {
        env.stack.push({ async: true });
    }
    return value;
};"#
            }
            Helper::DisposeResources => {
                r#"var __disposeResources = (this && this.__disposeResources) || (function (SuppressedError) {
    return function (env) {
        function fail(e) {
            env.error = env.hasError ? new SuppressedError(e, env.error, "An error was suppressed during disposal.") : e;
            env.hasError = true;
        }
        var r, s = 0;
        function next() {
            while (r = env.stack.pop()) {
                try {
                    if (!r.async && s === 1) return s = 0, env.stack.push(r), Promise.resolve().then(next);
                    if (r.dispose) {
                        var result = r.dispose.call(r.value);
                        if (r.async) return s |= 2, Promise.resolve(result).then(next, function(e) { fail(e); return next(); });
                    }
                    else s |= 1;
                }
                catch (e) {
                    fail(e);
                }
            }
            if (s === 1) return env.hasError ? Promise.reject(env.error) : Promise.resolve();
            if (env.hasError) throw env.error;
        }
        return next();
    };
})(typeof SuppressedError === "function" ? SuppressedError : function (error, suppressed, message) {
    var e = new Error(message);
    return e.name = "SuppressedError", e.error = error, e.suppressed = suppressed, e;
});"#
            }
            Helper::Awaiter => {
                r#"var __awaiter = (this && this.__awaiter) || function (thisArg, _arguments, P, generator) {
    function adopt(value) { return value instanceof P ? value : new P(function (resolve) { resolve(value); }); }
    return new (P || (P = Promise))(function (resolve, reject) {
        function fulfilled(value) { try { step(generator.next(value)); } catch (e) { reject(e); } }
        function rejected(value) { try { step(generator["throw"](value)); } catch (e) { reject(e); } }
        function step(result) { result.done ? resolve(result.value) : adopt(result.value).then(fulfilled, rejected); }
        step((generator = generator.apply(thisArg, _arguments || [])).next());
    });
};"#
            }
            Helper::Rest => {
                r#"var __rest = (this && this.__rest) || function (s, e) {
    var t = {};
    for (var p in s) if (Object.prototype.hasOwnProperty.call(s, p) && e.indexOf(p) < 0)
        t[p] = s[p];
    if (s != null && typeof Object.getOwnPropertySymbols === "function")
        for (var i = 0, p = Object.getOwnPropertySymbols(s); i < p.length; i++) {
            if (e.indexOf(p[i]) < 0 && Object.prototype.propertyIsEnumerable.call(s, p[i]))
                t[p[i]] = s[p[i]];
        }
    return t;
};"#
            }
            Helper::Await => {
                r#"var __await = (this && this.__await) || function (v) { return this instanceof __await ? (this.v = v, this) : new __await(v); }"#
            }
            Helper::AsyncGenerator => {
                r#"var __asyncGenerator = (this && this.__asyncGenerator) || function (thisArg, _arguments, generator) {
    if (!Symbol.asyncIterator) throw new TypeError("Symbol.asyncIterator is not defined.");
    var g = generator.apply(thisArg, _arguments || []), i, q = [];
    return i = Object.create((typeof AsyncIterator === "function" ? AsyncIterator : Object).prototype), verb("next"), verb("throw"), verb("return", awaitReturn), i[Symbol.asyncIterator] = function () { return this; }, i;
    function awaitReturn(f) { return function (v) { return Promise.resolve(v).then(f, reject); }; }
    function verb(n, f) { if (g[n]) { i[n] = function (v) { return new Promise(function (a, b) { q.push([n, v, a, b]) > 1 || resume(n, v); }); }; if (f) i[n] = f(i[n]); } }
    function resume(n, v) { try { step(g[n](v)); } catch (e) { settle(q[0][3], e); } }
    function step(r) { r.value instanceof __await ? Promise.resolve(r.value.v).then(fulfill, reject) : settle(q[0][2], r); }
    function fulfill(value) { resume("next", value); }
    function reject(value) { resume("throw", value); }
    function settle(f, v) { if (f(v), q.shift(), q.length) resume(q[0][0], q[0][1]); }
};"#
            }
            Helper::AsyncValues => {
                r#"var __asyncValues = (this && this.__asyncValues) || function (o) {
    if (!Symbol.asyncIterator) throw new TypeError("Symbol.asyncIterator is not defined.");
    var m = o[Symbol.asyncIterator], i;
    return m ? m.call(o) : (o = typeof __values === "function" ? __values(o) : o[Symbol.iterator](), i = {}, verb("next"), verb("throw"), verb("return"), i[Symbol.asyncIterator] = function () { return this; }, i);
    function verb(n) { i[n] = o[n] && function (v) { return new Promise(function (resolve, reject) { v = o[n](v), settle(resolve, reject, v.done, v.value); }); }; }
    function settle(resolve, reject, d, v) { Promise.resolve(v).then(function(v) { resolve({ value: v, done: d }); }, reject); }
};"#
            }
            Helper::AsyncDelegator => {
                r#"var __asyncDelegator = (this && this.__asyncDelegator) || function (o) {
    var i, p;
    return i = {}, verb("next"), verb("throw", function (e) { throw e; }), verb("return"), i[Symbol.iterator] = function () { return this; }, i;
    function verb(n, f) { i[n] = o[n] ? function (v) { return (p = !p) ? { value: __await(o[n](v)), done: false } : f ? f(v) : v; } : f; }
};"#
            }
            Helper::ClassPrivateFieldGet => {
                r#"var __classPrivateFieldGet = (this && this.__classPrivateFieldGet) || function (receiver, state, kind, f) {
    if (kind === "a" && !f) throw new TypeError("Private accessor was defined without a getter");
    if (typeof state === "function" ? receiver !== state || !f : !state.has(receiver)) throw new TypeError("Cannot read private member from an object whose class did not declare it");
    return kind === "m" ? f : kind === "a" ? f.call(receiver) : f ? f.value : state.get(receiver);
};"#
            }
            Helper::ClassPrivateFieldSet => {
                r#"var __classPrivateFieldSet = (this && this.__classPrivateFieldSet) || function (receiver, state, value, kind, f) {
    if (kind === "m") throw new TypeError("Private method is not writable");
    if (kind === "a" && !f) throw new TypeError("Private accessor was defined without a setter");
    if (typeof state === "function" ? receiver !== state || !f : !state.has(receiver)) throw new TypeError("Cannot write private member to an object whose class did not declare it");
    return (kind === "a" ? f.call(receiver, value) : f ? f.value = value : state.set(receiver, value)), value;
};"#
            }
            Helper::MakeTemplateObject => {
                r#"var __makeTemplateObject = (this && this.__makeTemplateObject) || function (cooked, raw) {
    if (Object.defineProperty) { Object.defineProperty(cooked, "raw", { value: raw }); } else { cooked.raw = raw; }
    return cooked;
};"#
            }
            Helper::ClassPrivateFieldIn => {
                r#"var __classPrivateFieldIn = (this && this.__classPrivateFieldIn) || function(state, receiver) {
    if (receiver === null || (typeof receiver !== "object" && typeof receiver !== "function")) throw new TypeError("Cannot use 'in' operator on non-object");
    return typeof state === "function" ? receiver === state : state.has(receiver);
};"#
            }
            Helper::CreateBinding => {
                r#"var __createBinding = (this && this.__createBinding) || (Object.create ? (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    var desc = Object.getOwnPropertyDescriptor(m, k);
    if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
      desc = { enumerable: true, get: function() { return m[k]; } };
    }
    Object.defineProperty(o, k2, desc);
}) : (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    o[k2] = m[k];
}));"#
            }
            Helper::SetModuleDefault => {
                r#"var __setModuleDefault = (this && this.__setModuleDefault) || (Object.create ? (function(o, v) {
    Object.defineProperty(o, "default", { enumerable: true, value: v });
}) : function(o, v) {
    o["default"] = v;
});"#
            }
            Helper::ImportStar => {
                r#"var __importStar = (this && this.__importStar) || (function () {
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
})();"#
            }
            Helper::ExportStar => {
                r#"var __exportStar = (this && this.__exportStar) || function(m, exports) {
    for (var p in m) if (p !== "default" && !Object.prototype.hasOwnProperty.call(exports, p)) __createBinding(exports, m, p);
};"#
            }
            Helper::ImportDefault => {
                r#"var __importDefault = (this && this.__importDefault) || function (mod) {
    return (mod && mod.__esModule) ? mod : { "default": mod };
};"#
            }
            Helper::Decorate => {
                r#"var __decorate = (this && this.__decorate) || function (decorators, target, key, desc) {
    var c = arguments.length, r = c < 3 ? target : desc === null ? desc = Object.getOwnPropertyDescriptor(target, key) : desc, d;
    if (typeof Reflect === "object" && typeof Reflect.decorate === "function") r = Reflect.decorate(decorators, target, key, desc);
    else for (var i = decorators.length - 1; i >= 0; i--) if (d = decorators[i]) r = (c < 3 ? d(r) : c > 3 ? d(target, key, r) : d(target, key)) || r;
    return c > 3 && r && Object.defineProperty(target, key, r), r;
};"#
            }
            Helper::Metadata => {
                r#"var __metadata = (this && this.__metadata) || function (k, v) {
    if (typeof Reflect === "object" && typeof Reflect.metadata === "function") return Reflect.metadata(k, v);
};"#
            }
            Helper::Param => {
                r#"var __param = (this && this.__param) || function (paramIndex, decorator) {
    return function (target, key) { decorator(target, key, paramIndex); }
};"#
            }
            Helper::RunInitializers => {
                r#"var __runInitializers = (this && this.__runInitializers) || function (thisArg, initializers, value) {
    var useValue = arguments.length > 2;
    for (var i = 0; i < initializers.length; i++) {
        value = useValue ? initializers[i].call(thisArg, value) : initializers[i].call(thisArg);
    }
    return useValue ? value : void 0;
};"#
            }
            Helper::EsDecorate => {
                r#"var __esDecorate = (this && this.__esDecorate) || function (ctor, descriptorIn, decorators, contextIn, initializers, extraInitializers) {
    function accept(f) { if (f !== void 0 && typeof f !== "function") throw new TypeError("Function expected"); return f; }
    var kind = contextIn.kind, key = kind === "getter" ? "get" : kind === "setter" ? "set" : "value";
    var target = !descriptorIn && ctor ? contextIn["static"] ? ctor : ctor.prototype : null;
    var descriptor = descriptorIn || (target ? Object.getOwnPropertyDescriptor(target, contextIn.name) : {});
    var _, done = false;
    for (var i = decorators.length - 1; i >= 0; i--) {
        var context = {};
        for (var p in contextIn) context[p] = p === "access" ? {} : contextIn[p];
        for (var p in contextIn.access) context.access[p] = contextIn.access[p];
        context.addInitializer = function (f) { if (done) throw new TypeError("Cannot add initializers after decoration has completed"); extraInitializers.push(accept(f || null)); };
        var result = (0, decorators[i])(kind === "accessor" ? { get: descriptor.get, set: descriptor.set } : descriptor[key], context);
        if (kind === "accessor") {
            if (result === void 0) continue;
            if (result === null || typeof result !== "object") throw new TypeError("Object expected");
            if (_ = accept(result.get)) descriptor.get = _;
            if (_ = accept(result.set)) descriptor.set = _;
            if (_ = accept(result.init)) initializers.unshift(_);
        }
        else if (_ = accept(result)) {
            if (kind === "field") initializers.unshift(_);
            else descriptor[key] = _;
        }
    }
    if (target) Object.defineProperty(target, contextIn.name, descriptor);
    done = true;
};"#
            }
            Helper::SetFunctionName => {
                r#"var __setFunctionName = (this && this.__setFunctionName) || function (f, name, prefix) {
    if (typeof name === "symbol") name = name.description ? "[".concat(name.description, "]") : "";
    return Object.defineProperty(f, "name", { configurable: true, value: prefix ? "".concat(prefix, " ", name) : name });
};"#
            }
            Helper::PropKey => {
                r#"var __propKey = (this && this.__propKey) || function (x) {
    return typeof x === "symbol" ? x : "".concat(x);
};"#
            }
            Helper::RewriteRelativeImportExtension => {
                r#"var __rewriteRelativeImportExtension = (this && this.__rewriteRelativeImportExtension) || function (path, preserveJsx) {
    if (typeof path === "string" && /^\.\.?\//.test(path)) {
        return path.replace(/\.(tsx)$|((?:\.d)?)((?:\.[^./]+?)?)\.([cm]?)ts$/i, function (m, tsx, d, ext, cm) {
            return tsx ? preserveJsx ? ".jsx" : ".js" : d && (!ext || !cm) ? m : (d + ext + "." + cm.toLowerCase() + "js");
        });
    }
    return path;
};"#
            }
        }
    }

    /// Helpers this one calls.
    fn deps(self) -> &'static [Helper] {
        match self {
            Helper::ImportStar => &[Helper::CreateBinding, Helper::SetModuleDefault],
            Helper::ExportStar => &[Helper::CreateBinding],
            Helper::AsyncGenerator => &[Helper::Await],
            Helper::AsyncDelegator => &[Helper::Await],
            _ => &[],
        }
    }
}

/// The set of helpers a file needs.
#[derive(Default, Debug)]
pub(crate) struct Helpers {
    used: u32,
    /// `__esDecorate` was requested before `__runInitializers` (tsc prints them in request
    /// order: class-only decorators ask for `__esDecorate` first).
    es_decorate_first: bool,
    /// [`Helper::REQUEST_ORDER`] helpers, in the order first asked for.
    requested: Vec<Helper>,
}

impl Helpers {
    pub fn add(&mut self, h: Helper) {
        if h == Helper::EsDecorate && !self.has(Helper::RunInitializers) && !self.has(h) {
            self.es_decorate_first = true;
        }
        for &d in h.deps() {
            self.add(d);
        }
        if !self.has(h) && Helper::REQUEST_ORDER.contains(&h) {
            self.requested.push(h);
        }
        self.used |= 1 << h as u32;
    }

    pub fn has(&self, h: Helper) -> bool {
        self.used & (1 << h as u32) != 0
    }

    /// Helper source text in tsc's order (or a `tslib` import with `importHelpers`).
    pub fn render(&self, opts: &TransformOptions) -> String {
        let mut used: Vec<Helper> = Helper::ALL.into_iter().filter(|&h| self.has(h)).collect();
        if self.es_decorate_first
            && let Some(i) = used.iter().position(|&h| h == Helper::RunInitializers)
            && used.get(i + 1) == Some(&Helper::EsDecorate)
        {
            used.swap(i, i + 1);
        }
        used.extend_from_slice(&self.requested);
        if used.is_empty() {
            return String::new();
        }
        if opts.import_helpers && !opts.is_cjs() {
            // Only helpers called directly need importing.
            let names: Vec<&str> = used
                .iter()
                .filter(|h| !matches!(h, Helper::CreateBinding | Helper::SetModuleDefault))
                .map(|h| h.name())
                .collect();
            return format!("import {{ {} }} from \"tslib\";\n", names.join(", "));
        }
        if opts.no_emit_helpers {
            return String::new();
        }
        let mut s = String::new();
        for h in used {
            s.push_str(h.text());
            s.push('\n');
        }
        s
    }
}
