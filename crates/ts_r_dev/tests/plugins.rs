//! Vite plugins through the Node sidecar: a `vite.config.mjs` with a virtual module,
//! pre/post transforms (one with a hook filter), `this.resolve`, `resolve.alias`, a
//! `.md` transform, `transformIndexHtml`, `configureServer` middlewares (pre and post, GET
//! and POST) and `handleHotUpdate` sending a custom event. Checked over HTTP, then in
//! headless Chrome (when present) including hot updates. A second test runs
//! vite-plugin-svgr and @vitejs/plugin-react's `jsxImportSource` with React in Chrome
//! (needs `TS_R_VITE_TOOLS` and `TS_R_REACT_ROOT`).
//!
//! Needs `node` on `PATH` (or `TS_R_NODE`); skipped with a message without it.

mod support;

ts_r_testing::cap_memory!();

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use support::{Chrome, DevProcess, find_chrome};
use ts_r_hmr::testutil::{TempDir, header, http_get};

const T: Duration = Duration::from_secs(20);

fn node_available() -> bool {
    let node = std::env::var_os("TS_R_NODE").unwrap_or_else(|| "node".into());
    Command::new(node)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

const CONFIG: &str = r#"import path from 'node:path'
const dir = import.meta.dirname
const order = []

export default ({ command }) => ({
  resolve: { alias: { '@lib': path.join(dir, 'src/lib') } },
  define: { __UNUSED__: '1' },
  plugins: [
    {
      name: 'test-virtual',
      buildStart() { order.push('virtual') },
      resolveId(id) { if (id === 'virtual:greeting') return '\0virtual:greeting' },
      load(id) { if (id === '\0virtual:greeting') return `export const greeting = ${JSON.stringify('hello ' + command)}` },
    },
    {
      name: 'test-pre',
      enforce: 'pre',
      // buildStart hooks start together; a sequential one waits for them.
      buildStart: { async handler() { await new Promise((r) => setTimeout(r, 50)); order.push('pre') } },
      transform(code, id) { if (id.endsWith('/typed.ts')) return code + '\nexport const fromPre: number = 41 + 1;\n' },
    },
    [[{
      name: 'test-post',
      transform: {
        filter: { id: /\.ts$/ },
        handler(code) { return code.replaceAll('__BUILD_TAG__', JSON.stringify('tag-42')) },
      },
    }], null, false],
    {
      name: 'test-resolve',
      buildStart: { sequential: true, handler() { order.push('sequential') } },
      async resolveId(id, importer) { if (id === '~helper') return this.resolve('./helper.ts', importer) },
    },
    {
      name: 'test-md',
      buildStart() { order.push('md') },
      transform(code, id) { if (id.endsWith('.md')) return 'export default ' + JSON.stringify(code.trim()) },
    },
    {
      name: 'test-html',
      transformIndexHtml: {
        order: 'pre',
        handler(html) {
          return {
            html: html.replace('%TITLE%', 'Plugged'),
            tags: [{ tag: 'meta', attrs: { name: 'ts-r-test', content: 'yes' }, injectTo: 'head' }],
          }
        },
      },
    },
    {
      name: 'test-server',
      configureServer(server) {
        server.middlewares.use('/api/hello', (req, res) => {
          res.setHeader('Content-Type', 'application/json')
          res.end(JSON.stringify({ method: req.method, url: req.url }))
        })
        server.middlewares.use('/api/order', (req, res) => res.end(order.join(',')))
        server.middlewares.use('/api/echo', (req, res) => {
          let body = ''
          req.on('data', (c) => { body += c })
          req.on('end', () => {
            res.statusCode = 201
            res.setHeader('X-Echo', 'yes')
            res.end(body.toUpperCase())
          })
        })
        server.ws.on('ping-plugin', (data) => server.ws.send('pong-plugin', { got: data.n }))
        return () => {
          server.middlewares.use((req, res, next) => {
            if (req.url === '/api/post-hook') res.end('post')
            else next()
          })
        }
      },
      hotUpdate({ file }) {
        if (file.endsWith('.hot')) {
          this.environment.hot.send({ type: 'custom', event: 'hot-env', data: path.basename(file) })
          return []
        }
      },
      handleHotUpdate({ file, server }) {
        if (file.endsWith('.txt')) {
          server.ws.send({ type: 'custom', event: 'notes-changed', data: { file: path.basename(file) } })
          return []
        }
      },
    },
    { name: 'build-only', apply: 'build', transform() { throw new Error('build-only plugin ran in serve') } },
  ],
})
"#;

const MAIN: &str = r#"import { greeting } from 'virtual:greeting'
import { tag } from './tagged.ts'
import { fromPre } from './typed.ts'
import { helper } from '~helper'
import { lib } from '@lib/util.ts'
import notes from './notes.md'
const w = window as any
w.__marker ??= 'page-' + Math.random()
document.getElementById('out')!.textContent = [greeting, tag, fromPre, helper, lib, notes].join('|')
if (import.meta.hot) {
  import.meta.hot.accept()
  import.meta.hot.on('notes-changed', (d: { file: string }) => { w.__notes = d.file })
  import.meta.hot.on('pong-plugin', (d: { got: number }) => { w.__pong = d.got })
  import.meta.hot.on('hot-env', (d: string) => { w.__hotEnv = d })
  w.__ping = (n: number) => import.meta.hot!.send('ping-plugin', { n })
}
"#;

fn write_app(app: &TempDir) {
    app.write(
        "index.html",
        "<!doctype html><html><head><title>%TITLE%</title></head><body><div id=\"out\"></div><script type=\"module\" src=\"/src/main.ts\"></script></body></html>\n",
    );
    app.write("vite.config.mjs", CONFIG);
    app.write("src/main.ts", MAIN);
    app.write(
        "src/tagged.ts",
        "export const tag: string = __BUILD_TAG__\n",
    );
    app.write("src/typed.ts", "export const typed = 1\n");
    app.write("src/helper.ts", "export const helper: string = 'helper'\n");
    // Reached only through the alias: the dependency scan must follow it.
    app.write(
        "src/lib/util.ts",
        "import { tiny } from 'tiny-dep'\nexport const lib: string = tiny\n",
    );
    app.write(
        "node_modules/tiny-dep/package.json",
        r#"{"name":"tiny-dep","type":"module","main":"index.js"}"#,
    );
    app.write(
        "node_modules/tiny-dep/index.js",
        "export const tiny = 'lib'\n",
    );
    app.write("src/notes.md", "# Notes\n");
}

fn post(addr: &str, path: &str, body: &str) -> (u16, String, String) {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(T)).unwrap();
    write!(
        s,
        "POST {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut resp = String::new();
    s.read_to_string(&mut resp).unwrap();
    let (head, body) = resp.split_once("\r\n\r\n").unwrap_or((&resp, ""));
    let status = head
        .split(' ')
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    (status, head.to_owned(), body.to_owned())
}

/// The log once a line containing `needle` arrived (startup lines can trail `Local:`).
fn log_after(dev: &DevProcess, needle: &str) -> String {
    let start = Instant::now();
    while !dev.log_lines().iter().any(|l| l.contains(needle)) && start.elapsed() < T {
        std::thread::sleep(Duration::from_millis(10));
    }
    dev.log_lines().join("\n")
}

fn sidecar_pid(dev: &DevProcess) -> Option<u32> {
    dev.log_lines().iter().find_map(|l| {
        let rest = l.split("Node sidecar pid ").nth(1)?;
        rest.split(',').next()?.trim().parse().ok()
    })
}

fn process_alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[test]
fn vite_plugins_run_in_a_node_sidecar() {
    if !node_available() {
        eprintln!("skipping: node not found (set TS_R_NODE)");
        return;
    }
    let app = TempDir::new("plugins");
    let cache = TempDir::new("plugins-cache");
    write_app(&app);
    let dev = DevProcess::start(app.path(), cache.path(), &[]);
    let addr = dev.addr.parse().unwrap();
    let log = log_after(&dev, "plugins: ");
    assert!(
        log.contains("plugins: test-pre, test-virtual, test-post, test-resolve, test-md, test-html, test-server from vite.config.mjs (Node sidecar pid"),
        "{log}"
    );
    // `define` is applied: its globals are served with the env.
    let (_, _, env) = http_get(addr, "/@vite/env", &[]);
    assert!(env.contains("__UNUSED__"), "{env}");
    let start = Instant::now();
    while !dev.log_lines().iter().any(|l| l.contains("deps: ")) && start.elapsed() < T {
        std::thread::sleep(Duration::from_millis(20));
    }
    let log = dev.log_lines().join("\n");
    assert!(
        log.contains("pre-bundled 1 entries"),
        "the scan follows aliases: {log}"
    );
    let pid = sidecar_pid(&dev).expect("sidecar pid logged");

    // transformIndexHtml: string edit and an injected tag.
    let (status, _, html) = http_get(addr, "/", &[]);
    assert_eq!(status, 200);
    assert!(html.contains("<title>Plugged</title>"), "{html}");
    assert!(
        html.contains("<meta name=\"ts-r-test\" content=\"yes\">"),
        "{html}"
    );

    // resolveId/load (virtual), this.resolve, alias, .md through a transform.
    let (status, _, main) = http_get(addr, "/src/main.ts", &[]);
    assert_eq!(status, 200, "{main}");
    for want in [
        "from '/@id/__x00__virtual:greeting'",
        "from '/src/tagged.ts'",
        "from '/src/helper.ts'",
        "from '/src/lib/util.ts'",
    ] {
        assert!(main.contains(want), "{want} in {main}");
    }
    let md_url = main
        .split(['"', '\''])
        .find(|s| s.starts_with("/@id/") && s.contains("notes.md"))
        .unwrap_or_else(|| panic!("notes.md plugin URL in {main}"))
        .to_owned();
    let (status, _, virt) = http_get(addr, "/@id/__x00__virtual:greeting", &[]);
    assert_eq!(status, 200);
    assert!(virt.contains("hello serve"), "{virt}");
    assert_eq!(
        http_get(addr, "/api/order", &[]).2,
        "virtual,pre,sequential,md"
    );
    let (status, _, md) = http_get(addr, &md_url, &[]);
    assert_eq!(status, 200, "{md}");
    assert!(md.contains("export default \"# Notes\""), "{md}");
    // Unknown /@id/ URLs aren't served.
    assert_eq!(http_get(addr, "/@id/%2Fetc%2Fpasswd", &[]).0, 404);

    // Post transform with a hook filter; pre transform output still goes through TS.
    let (_, _, tagged) = http_get(addr, "/src/tagged.ts", &[]);
    assert!(tagged.contains("export const tag = \"tag-42\""), "{tagged}");
    let (_, _, typed) = http_get(addr, "/src/typed.ts", &[]);
    assert!(typed.contains("export const fromPre = 41 + 1"), "{typed}");
    assert!(!typed.contains(": number"), "{typed}");

    // configureServer middlewares: mounted route (prefix stripped), POST body, post hook.
    let (status, headers, body) = http_get(addr, "/api/hello?x=1", &[]);
    assert_eq!(status, 200);
    assert_eq!(header(&headers, "content-type"), Some("application/json"));
    assert_eq!(body, r#"{"method":"GET","url":"/?x=1"}"#);
    let (status, head, body) = post(&dev.addr, "/api/echo", "abc");
    assert_eq!(status, 201, "{head}");
    assert!(head.to_ascii_lowercase().contains("x-echo: yes"), "{head}");
    assert_eq!(body, "ABC");
    assert_eq!(http_get(addr, "/api/post-hook", &[]).2, "post");
    assert_eq!(http_get(addr, "/api/nothing.js", &[]).0, 404);

    if let Some(chrome_bin) = find_chrome() {
        let mut chrome = Chrome::launch(&chrome_bin);
        chrome.open(&dev.url);
        let cdp = &mut chrome.cdp;
        cdp.wait_for(
            "document.getElementById('out')?.textContent === 'hello serve|tag-42|42|helper|lib|# Notes'",
            T,
        );
        cdp.wait_for("typeof window.__ping === 'function'", T);
        std::thread::sleep(Duration::from_millis(300));
        let marker = cdp.eval("window.__marker").unwrap();
        // handleHotUpdate: a custom event, no update.
        app.write("src/data.txt", "x");
        cdp.wait_for("window.__notes === 'data.txt'", T);
        // hotUpdate: `this.environment.hot` sends to the page.
        app.write("src/data.hot", "x");
        cdp.wait_for("window.__hotEnv === 'data.hot'", T);
        // A plugin module's file changes: it's re-loaded and re-transformed.
        app.write("src/notes.md", "# Notes v2\n");
        cdp.wait_for(
            "document.getElementById('out')?.textContent.endsWith('|# Notes v2')",
            T,
        );
        // A post-transformed module.
        app.write(
            "src/tagged.ts",
            "export const tag: string = __BUILD_TAG__ + '!'\n",
        );
        cdp.wait_for(
            "document.getElementById('out')?.textContent.includes('|tag-42!|')",
            T,
        );
        // server.ws.on: a page's custom event reaches the plugin, which answers.
        cdp.eval("window.__ping(7)").unwrap();
        cdp.wait_for("window.__pong === 7", T);
        assert_eq!(
            cdp.eval(&format!("window.__marker === {marker}")).unwrap(),
            true,
            "page reloaded"
        );
    } else {
        eprintln!("Chrome not found: skipping the browser half (set TS_R_CHROME)");
    }

    for l in dev.log_lines() {
        eprintln!("{l}");
    }
    let log = dev.log_lines().join("\n");
    assert!(!log.contains("new dependency"), "{log}");
    // The sidecar goes away with the server (ts-r-dev is SIGKILLed here: the sidecar sees
    // its stdin close).
    drop(dev);
    let start = Instant::now();
    while process_alive(pid) && start.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!process_alive(pid), "sidecar {pid} outlived ts-r-dev");
}

/// `transform` hooks get Vite's module type (`json` before the JSON step, `js` after, `ts`
/// for TypeScript), JSON files and JSON-id plugin modules go through them, and a filter an
/// `options` hook sets is the one used.
#[test]
fn transform_hooks_see_module_types_and_options_hook_filters() {
    if !node_available() {
        eprintln!("skipping: node not found (set TS_R_NODE)");
        return;
    }
    let app = TempDir::new("plugins-module-type");
    let cache = TempDir::new("plugins-module-type-cache");
    app.write("index.html", "<!doctype html><body>hi</body>\n");
    app.write(
        "vite.config.mjs",
        r#"const types = ['pre', 'post'].map((enforce) => ({
  name: `type-${enforce}`,
  enforce,
  transform(code, id, opts) {
    if (id.endsWith('.json') || id.endsWith('/a.ts')) return code.replace(`TYPE_${enforce.toUpperCase()}`, opts.moduleType)
  },
}))
const lazy = {
  name: 'lazy',
  options() { lazy.transform.filter = { id: '**/a.ts' } },
  transform: { filter: { id: { exclude: ['**/*.ts'] } }, handler: (code) => code.replace('LAZY', 'ok') },
}
const typed = { name: 'typed', transform: { filter: { moduleType: ['json'] }, handler() { throw new Error('json is js after the JSON step') } } }
export default {
  plugins: [types, lazy, typed, {
    name: 'virtual-json',
    resolveId(id) { if (id === 'virtual:/v.json') return '\0/v.json' },
    load(id) { if (id === '\0/v.json') return JSON.stringify({ pre: 'TYPE_PRE', post: 'TYPE_POST' }) },
  }],
}
"#,
    );
    app.write(
        "src/a.ts",
        "import d from './d.json'\nimport v from 'virtual:/v.json'\nexport const x: string = 'TYPE_PRE TYPE_POST LAZY'\nexport const both = [d, v]\n",
    );
    app.write("src/d.json", r#"{"pre":"TYPE_PRE","post":"TYPE_POST"}"#);
    let dev = DevProcess::start(app.path(), cache.path(), &[]);
    let addr = dev.addr.parse().unwrap();
    let (status, _, a) = http_get(addr, "/src/a.ts", &[]);
    assert_eq!(status, 200, "{a}");
    assert!(a.contains("'ts js ok'"), "{a}");
    let (status, _, d) = http_get(addr, "/src/d.json?import", &[]);
    assert_eq!(status, 200, "{d}");
    assert!(d.contains(r#"export const pre = "json";"#), "{d}");
    assert!(d.contains(r#"export const post = "js";"#), "{d}");
    let (status, _, v) = http_get(addr, "/@id/__x00__/v.json", &[]);
    assert_eq!(status, 200, "{v}");
    assert!(v.contains(r#"export const pre = "json";"#), "{v}");
    assert!(v.contains(r#"export const post = "js";"#), "{v}");
}

/// `enforce: 'post'` plugins and `order: 'post'` hooks see `import.meta.glob` and variable
/// dynamic imports expanded (Vite runs them after those steps); normal-order hooks see them
/// as written. The variable import's helper module is served.
#[test]
fn late_transform_hooks_see_expanded_globs() {
    if !node_available() {
        eprintln!("skipping: node not found (set TS_R_NODE)");
        return;
    }
    let app = TempDir::new("plugins-late");
    let cache = TempDir::new("plugins-late-cache");
    app.write("index.html", "<!doctype html><body>hi</body>\n");
    app.write(
        "vite.config.mjs",
        r#"const mark = (name, extra) => ({
  name,
  ...extra,
  transform: {
    ...(extra.order ? { order: extra.order } : {}),
    handler(code, id) {
      if (!id.endsWith('/main.js')) return
      const seen = code.includes('import.meta.glob') ? 'raw' : code.includes('__variableDynamicImportRuntimeHelper') ? 'expanded' : 'none'
      return code + `\nexport const ${name} = '${seen}'`
    },
  },
})
export default {
  plugins: [mark('normal', {}), mark('enforced', { enforce: 'post' }), mark('ordered', { order: 'post' }), mark('early', { enforce: 'pre', order: 'post' })],
}
"#,
    );
    app.write(
        "src/main.js",
        "const name = 'a'\nexport const globbed = import.meta.glob('./dir/*.js')\nexport const loaded = import(`./dir/${name}.js`)\n",
    );
    app.write("src/dir/a.js", "export default 'a'\n");
    app.write("src/dir/b.js", "export default 'b'\n");
    let dev = DevProcess::start(app.path(), cache.path(), &[]);
    let addr = dev.addr.parse().unwrap();
    let (status, _, main) = http_get(addr, "/src/main.js", &[]);
    assert_eq!(status, 200, "{main}");
    assert!(main.contains("export const normal = 'raw'"), "{main}");
    assert!(
        main.contains("export const enforced = 'expanded'"),
        "{main}"
    );
    assert!(main.contains("export const ordered = 'expanded'"), "{main}");
    assert!(main.contains("export const early = 'expanded'"), "{main}");
    assert!(main.contains("/src/dir/a.js"), "{main}");
    assert!(main.contains("`./dir/${name}.js`, 3)"), "{main}");
    let (status, _, helper) = http_get(addr, "/@id/__x00__vite/dynamic-import-helper.js", &[]);
    assert_eq!(status, 200, "{helper}");
    assert!(
        helper.contains("Unknown variable dynamic import: "),
        "{helper}"
    );
}

/// An ES module `vite.config.js` in a `"type": "commonjs"` package loads (Vite bundles it;
/// the sidecar retries it as `.mjs`), and `this.addWatchFile` in an unfiltered `load` hook and
/// in `transform` makes editing those files run the module's hooks again.
#[test]
fn esm_config_in_a_commonjs_package_and_plugin_watch_files() {
    if !node_available() {
        eprintln!("skipping: node not found (set TS_R_NODE)");
        return;
    }
    let app = TempDir::new("plugins-cjs-watch");
    let cache = TempDir::new("plugins-cjs-watch-cache");
    app.write("package.json", r#"{ "type": "commonjs" }"#);
    app.write(
        "index.html",
        "<!doctype html><script type=\"module\" src=\"/src/main.js\"></script>\n",
    );
    app.write("src/main.js", "console.log(RUNS);\n");
    app.write("dep.txt", "a");
    app.write("load-dep.txt", "a");
    app.write(
        "vite.config.js",
        r#"import path from 'node:path'
let runs = 0
const dir = import.meta.dirname
export default {
  plugins: [{
    name: 'watcher',
    load(id) { if (id.endsWith('/src/main.js')) this.addWatchFile('./load-dep.txt') },
    transform(code, id) {
      if (!id.endsWith('/src/main.js')) return
      this.addWatchFile(path.join(dir, 'dep.txt'))
      return `const RUNS = ${++runs};\n` + code
    },
  }],
}
"#,
    );
    let dev = DevProcess::start(app.path(), cache.path(), &[]);
    let addr = dev.addr.parse().unwrap();
    let runs = |n: u32| {
        let want = format!("const RUNS = {n};");
        let deadline = Instant::now() + T;
        loop {
            let (status, _, body) = http_get(addr, "/src/main.js", &[]);
            if status == 200 && body.contains(&want) {
                return;
            }
            assert!(Instant::now() < deadline, "no {want}: {status} {body}");
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    runs(1);
    runs(1);
    app.write("dep.txt", "b");
    runs(2);
    app.write("load-dep.txt", "b");
    runs(3);
    assert!(
        std::fs::read_dir(app.path()).unwrap().all(|e| !e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains("timestamp")),
        "the temporary config copy is removed"
    );
}

#[test]
fn proxy_bypass_functions_run_in_the_sidecar() {
    if !node_available() {
        eprintln!("skipping: node not found (set TS_R_NODE)");
        return;
    }
    let app = TempDir::new("plugins-bypass");
    let cache = TempDir::new("plugins-bypass-cache");
    app.write("index.html", "<!doctype html><body>hi</body>\n");
    app.write("local.txt", "served locally\n");
    app.write(
        "vite.config.mjs",
        r#"const wait = (ms) => new Promise((r) => setTimeout(r, ms))
export default {
  server: {
    proxy: {
      '/missing': { target: 'http://127.0.0.1:9', bypass: () => false },
      '/written': {
        bypass: async (req, res) => {
          await wait(4)
          res.writeHead(201, { 'Content-Type': 'text/plain' })
          res.end('written by ' + req.url)
          return '/written'
        },
      },
      '/thrown': { bypass: async () => { await wait(4); throw new Error('bypass failed here') } },
      '/elsewhere': { target: 'http://127.0.0.1:9', bypass: (req) => (req.headers.accept === 'text/plain' ? '/local.txt' : undefined) },
    },
  },
}
"#,
    );
    let dev = DevProcess::start(app.path(), cache.path(), &[]);
    let addr = dev.addr.parse().unwrap();
    let (status, headers, body) = http_get(addr, "/missing/app", &[]);
    assert_eq!(status, 404, "{body}");
    assert!(
        body.is_empty() && header(&headers, "content-type").is_none(),
        "{headers:?}"
    );
    let (status, _, body) = http_get(addr, "/written?x=1", &[]);
    assert_eq!((status, body.as_str()), (201, "written by /written?x=1"));
    let (status, _, _) = http_get(addr, "/thrown", &[]);
    assert_eq!(status, 500);
    assert!(log_after(&dev, "bypass failed here").contains("bypass failed here"));
    let (status, _, body) = http_get(addr, "/elsewhere/x", &[("accept", "text/plain")]);
    assert_eq!((status, body.as_str()), (200, "served locally\n"));
    // Not bypassed: proxied to a closed port.
    let (status, _, _) = http_get(addr, "/elsewhere/x", &[]);
    assert_eq!(status, 502);
}

#[test]
fn react_only_configs_skip_the_sidecar_once_the_manifest_is_cached() {
    if !node_available() {
        eprintln!("skipping: node not found (set TS_R_NODE)");
        return;
    }
    let app = TempDir::new("plugins-react-only");
    let cache = TempDir::new("plugins-react-only-cache");
    app.write("index.html", "<!doctype html><body>hi</body>\n");
    // Stands in for @vitejs/plugin-react: same plugin names, a config hook setting JSX.
    app.write(
        "vite.config.mjs",
        "const react = (o) => [{ name: 'vite:react-babel', enforce: 'pre', config: () => ({ oxc: { jsx: { runtime: 'automatic', importSource: o.jsxImportSource } } }), transform() { throw new Error('must not run') } }, { name: 'vite:react-refresh', transformIndexHtml() { throw new Error('must not run') } }]\nexport default { plugins: [react({ jsxImportSource: 'my-jsx' })] }\n",
    );
    app.write("src/a.jsx", "export const A = () => <b>a</b>\n");
    app.write(
        "node_modules/my-jsx/package.json",
        r#"{"name":"my-jsx","type":"module","exports":{"./jsx-runtime":"./rt.js","./jsx-dev-runtime":"./rt.js"}}"#,
    );
    app.write(
        "node_modules/my-jsx/rt.js",
        "export const jsx = () => null, jsxs = jsx, jsxDEV = jsx, Fragment = {};\n",
    );
    let first = DevProcess::start(app.path(), cache.path(), &[]);
    let addr = first.addr.parse().unwrap();
    let (status, _, code) = http_get(addr, "/src/a.jsx", &[]);
    assert_eq!(status, 200, "{code}");
    assert!(code.contains("my-jsx_jsx-runtime"), "{code}");
    let log = log_after(&first, "plugins:");
    assert!(
        log.contains(
            "plugins: none from vite.config.mjs; built in: vite:react-babel, vite:react-refresh"
        ),
        "{log}"
    );
    assert!(!log.contains("Node sidecar"), "{log}");
    drop(first);
    // Warm: the manifest comes from the cache, Node doesn't start.
    let t = Instant::now();
    let second = DevProcess::start(app.path(), cache.path(), &[]);
    let warm = t.elapsed();
    let log = log_after(&second, "plugins:");
    assert!(log.contains("built in: vite:react-babel"), "{log}");
    let (_, _, html) = http_get(second.addr.parse().unwrap(), "/", &[]);
    assert!(html.contains("hi"));
    eprintln!("warm start with a cached plugin manifest: {warm:?}");
}

fn link(target: &Path, at: &Path) {
    if let Some(p) = at.parent() {
        std::fs::create_dir_all(p).unwrap();
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, at).unwrap();
    #[cfg(not(unix))]
    let _ = target;
}

/// vite-plugin-svgr (`?react` imports, loaded by the plugin and compiled with the
/// project's Vite) and @vitejs/plugin-react's `jsxImportSource`, rendered by React.
#[test]
fn svgr_and_plugin_react_options_in_chrome() {
    if !node_available() {
        eprintln!("skipping: node not found (set TS_R_NODE)");
        return;
    }
    let (Some(tools), Some(react)) = (
        std::env::var_os("TS_R_VITE_TOOLS").map(PathBuf::from),
        std::env::var_os("TS_R_REACT_ROOT").map(PathBuf::from),
    ) else {
        eprintln!(
            "skipping: set TS_R_VITE_TOOLS (node_modules with vite, vite-plugin-svgr, @vitejs/plugin-react) and TS_R_REACT_ROOT (react, react-dom, react-refresh)"
        );
        return;
    };
    let Some(chrome_bin) = find_chrome() else {
        eprintln!("skipping: Chrome not found (set TS_R_CHROME)");
        return;
    };
    let app = TempDir::new("plugins-svgr");
    let cache = TempDir::new("plugins-svgr-cache");
    let nm = app.path().join("node_modules");
    for p in ["vite", "vite-plugin-svgr", "@vitejs/plugin-react"] {
        link(&tools.join("node_modules").join(p), &nm.join(p));
    }
    for p in ["react", "react-dom", "scheduler", "react-refresh"] {
        link(&react.join("node_modules").join(p), &nm.join(p));
    }
    // A JSX runtime that marks every DOM element it creates.
    let runtime = |inner: &str, names: &str| {
        format!(
            "import * as r from '{inner}';\nexport const Fragment = r.Fragment;\nconst mark = (t, p) => (typeof t === 'string' ? {{ ...p, 'data-my-jsx': '' }} : p);\n{}",
            names
                .split(',')
                .map(|n| format!(
                    "export const {n} = (t, p, ...rest) => r.{n}(t, mark(t, p), ...rest);\n"
                ))
                .collect::<String>()
        )
    };
    app.write(
        "node_modules/my-jsx/package.json",
        r#"{"name":"my-jsx","type":"module","exports":{"./jsx-runtime":"./jsx-runtime.js","./jsx-dev-runtime":"./jsx-dev-runtime.js"}}"#,
    );
    app.write(
        "node_modules/my-jsx/jsx-runtime.js",
        &runtime("react/jsx-runtime", "jsx,jsxs"),
    );
    app.write(
        "node_modules/my-jsx/jsx-dev-runtime.js",
        &runtime("react/jsx-dev-runtime", "jsxDEV"),
    );
    app.write(
        "vite.config.mjs",
        "import { defineConfig } from 'vite'\nimport react from '@vitejs/plugin-react'\nimport svgr from 'vite-plugin-svgr'\nexport default defineConfig({ plugins: [react({ jsxImportSource: 'my-jsx' }), svgr()] })\n",
    );
    app.write(
        "index.html",
        "<!doctype html><body><div id=\"root\"></div><script type=\"module\" src=\"/src/main.jsx\"></script></body>\n",
    );
    app.write(
        "src/main.jsx",
        "import { createRoot } from 'react-dom/client';\nimport App from './App.jsx';\nwindow.__marker ??= 'page-' + Math.random();\ncreateRoot(document.getElementById('root')).render(<App />);\n",
    );
    app.write(
        "src/App.jsx",
        "import { useState } from 'react';\nimport Logo from './logo.svg?react';\nimport logoUrl from './logo.svg';\nexport default function App() {\n  const [n, setN] = useState(0);\n  return <div><button id=\"b\" onClick={() => setN(n + 1)}>{'clicks ' + n}</button><Logo id=\"logo\" width={20} /><img id=\"img\" src={logoUrl} /></div>;\n}\n",
    );
    let svg = |fill: &str| {
        format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 10 10\"><circle cx=\"5\" cy=\"5\" r=\"4\" fill=\"{fill}\"/></svg>\n"
        )
    };
    app.write("src/logo.svg", &svg("red"));

    let dev = DevProcess::start(app.path(), cache.path(), &[]);
    let log = log_after(&dev, "plugins:");
    assert!(
        log.contains("plugins: vite-plugin-svgr from vite.config.mjs"),
        "{log}"
    );
    assert!(log.contains("built in: vite:react-babel"), "{log}");
    let mut chrome = Chrome::launch(&chrome_bin);
    chrome.open(&dev.url);
    let cdp = &mut chrome.cdp;
    // svgr's output imports react/jsx-runtime, which the dependency scan can't see (only
    // my-jsx imports it): the first load finds it and reloads the page once.
    let start = Instant::now();
    while !dev.log_lines().iter().any(|l| l.contains("re-bundled")) && start.elapsed() < T {
        std::thread::sleep(Duration::from_millis(50));
    }
    // Start from a fresh page once the dependencies settled.
    std::thread::sleep(Duration::from_millis(200));
    cdp.eval("window.__old = 1").unwrap();
    cdp.call("Page.navigate", serde_json::json!({ "url": dev.url }))
        .unwrap();
    cdp.wait_for(
        "!window.__old && document.getElementById('b')?.textContent === 'clicks 0'",
        T,
    );
    // A late dependency re-bundle reloads the page: wait until a page survives 500 ms.
    let start = Instant::now();
    loop {
        cdp.wait_for(
            "document.getElementById('b')?.textContent === 'clicks 0'",
            T,
        );
        let _ = cdp.eval("window.__settled = 1");
        std::thread::sleep(Duration::from_millis(500));
        if cdp.eval("window.__settled === 1") == Ok(serde_json::Value::Bool(true))
            || start.elapsed() > T
        {
            break;
        }
    }
    // svgr rendered the SVG as a component; plain `.svg` imports are still URLs.
    cdp.wait_for(
        "document.querySelector('svg#logo circle')?.getAttribute('fill') === 'red'",
        T,
    );
    assert_eq!(
        cdp.eval("document.getElementById('img').getAttribute('src')")
            .unwrap(),
        "/src/logo.svg"
    );
    // jsxImportSource: our runtime created the elements.
    assert_eq!(
        cdp.eval("document.getElementById('b').hasAttribute('data-my-jsx')")
            .unwrap(),
        true
    );
    cdp.eval("document.getElementById('b').click()").unwrap();
    cdp.wait_for(
        "document.getElementById('b')?.textContent === 'clicks 1'",
        T,
    );
    let marker = cdp.eval("window.__marker").unwrap();
    // Editing the SVG re-runs svgr; App (a Fast Refresh boundary) keeps its state.
    app.write("src/logo.svg", &svg("blue"));
    cdp.wait_for(
        "document.querySelector('svg#logo circle')?.getAttribute('fill') === 'blue'",
        T,
    );
    let clicks = cdp
        .eval("document.getElementById('b').textContent")
        .unwrap();
    if clicks != "clicks 1" {
        for l in dev.log_lines() {
            eprintln!("{l}");
        }
    }
    assert_eq!(clicks, "clicks 1");
    assert_eq!(
        cdp.eval(&format!("window.__marker === {marker}")).unwrap(),
        true
    );
    for l in dev.log_lines() {
        eprintln!("{l}");
    }
    drop(chrome);
    drop(dev);

    // Warm restart: the hooks come from the cached manifest and Node starts in the
    // background; the first requests wait for it.
    let dev = DevProcess::start(app.path(), cache.path(), &[]);
    let log = log_after(&dev, "plugins:");
    assert!(log.contains("starting in the background"), "{log}");
    let mut chrome = Chrome::launch(&chrome_bin);
    chrome.open(&dev.url);
    chrome.cdp.wait_for(
        "document.querySelector('svg#logo circle')?.getAttribute('fill') === 'blue'",
        T,
    );
    for l in dev.log_lines() {
        eprintln!("{l}");
    }
}
