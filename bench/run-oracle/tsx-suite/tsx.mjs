// A stand-in for tsx's tests/utils/tsx.ts: the same exports, but `tsx(args, cwd)` runs the
// subject the oracle is measuring. TS_R_ORACLE_SUBJECT=tsx runs tsx 4.23.15's CLI
// (bench/node_modules/tsx, the reference) exactly as upstream does (execaNode, TSX_DISABLE_CACHE,
// DEBUG); TS_R_ORACLE_SUBJECT=ts-r-run runs the ts-r-run binary (TS_R_ORACLE_TSR) with the same
// arguments, cwd and env. `supports` describes Node 24 (the oracle's Node); every flag is set
// by hand because tsx's own capability table is tsx source, which the oracle never loads.
// Tests that use tsx's loader, API or CLI path directly are skipped before they run.

import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { execa, execaNode } from 'execa';
import { onTestFail, onTestFinish } from './manten.mjs';

const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../..');
const tsxDist = path.join(repo, 'bench/node_modules/tsx/dist');
const subject = process.env.TS_R_ORACLE_SUBJECT ?? 'tsx';
const tsrRun = process.env.TS_R_ORACLE_TSR;

export const tsxPath = path.join(tsxDist, 'cli.mjs');
export const tsxCjsPath = path.join(tsxDist, 'cjs/index.cjs');
export const tsxCjsApiPath = path.join(tsxDist, 'cjs/api/index.cjs');
export const tsxEsmPath = new URL('file://' + path.join(tsxDist, 'esm/index.mjs')).toString();
export const tsxEsmApiPath = new URL('file://' + path.join(tsxDist, 'esm/api/index.mjs')).toString();
export const tsxEsmApiCjsPath = path.join(tsxDist, 'esm/api/index.cjs');

const run = (args, options) => {
  const env = {
    TSX_DISABLE_CACHE: '1',
    DEBUG: '1',
    ...options.env,
  };
  if (subject === 'tsx') {
    return execaNode(tsxPath, args, {
      nodePath: options.nodePath ?? process.execPath,
      nodeOptions: [],
      reject: false,
      all: true,
      ...options,
      env,
    });
  }
  const { nodePath, nodeOptions, ...rest } = options;
  return execa(tsrRun, args, { reject: false, all: true, ...rest, env });
};

const track = (child) => {
  let result;
  let disposed;
  const dispose = () => {
    disposed ??= (async () => {
      if (child.exitCode === null) child.kill('SIGKILL');
      result = await child;
    })();
    return disposed;
  };
  // A failing test's report carries what the subject printed (the last process it started).
  onTestFail(async (error) => {
    await dispose();
    if (error && typeof error === 'object') {
      const out = `exit ${result.exitCode ?? result.signal}: ${result.all ?? ''}`;
      error.oracleOutput = out.slice(0, 3000);
    }
  });
  onTestFinish(dispose);
  return child;
};

/** The old single-call API (`tsx({ args, cwd })`). */
export const tsx = (options) => track(run(options.args, { cwd: options.cwd, nodePath: options.nodePath }));

export const createNode = async () => {
  const supports = {
    moduleRegister: true,
    moduleRegisterHooksCjsReload: true,
    esmLoadReadFile: true,
    importMetaPathProperties: true,
    testRunnerGlob: true,
    cliTestFlag: true,
    cjsInterop: true,
    cjsNamespaceModuleExports: true,
    nativeTypeScript: true,
    wasmModules: true,
    requireEsm: true,
    requireEsmNoWarning: true,
    requireEsmExtensionlessMjs: true,
    modulePackageMainResolution: true,
  };
  const skip = (what) => {
    const error = new Error(`uses ${what}`);
    error.oracleSkip = `tsx-only: ${what}`;
    throw error;
  };
  return {
    version: process.versions.node,
    path: process.execPath,
    supports,
    tsx: (args, cwdOrOptions) => {
      const isCwd = typeof cwdOrOptions === 'string';
      const options = isCwd ? { cwd: cwdOrOptions } : { ...cwdOrOptions };
      return track(run(args, options));
    },
    cjsPatched: () => skip("tsx's CommonJS register (--require tsx/cjs)"),
    hook: () => skip("tsx's ESM loader (--import tsx)"),
  };
};
