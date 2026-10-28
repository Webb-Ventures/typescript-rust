ts_r_testing::cap_memory!();

use std::path::PathBuf;
use std::sync::Arc;

use ts_r_bundler::fscache::CachedFs;
use ts_r_bundler::prepare::load;
use ts_r_bundler::{BuildContext, BuildOptions, EntryPoint, Loader};
use ts_r_config::{FileSystem, RealFs};

#[test]
fn binary_loaders_preserve_original_file_bytes() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("binary-loader-bytes");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let fs = Arc::new(RealFs::new());
    let opts = BuildOptions::default();
    for (name, bytes, base64) in [
        ("invalid.bin", vec![0xff], "/w=="),
        ("utf16le.bin", vec![0xff, 0xfe, b'x', 0], "//54AA=="),
        ("utf16be.bin", vec![0xfe, 0xff, 0, b'x'], "/v8AeA=="),
    ] {
        let file = dir.join(name);
        std::fs::write(&file, &bytes).expect("write");
        let path = file.to_str().expect("path");
        assert_eq!(fs.read_bytes(path).as_deref(), Some(bytes.as_slice()));
        assert_eq!(
            FileSystem::read_bytes(&&*fs, path).as_deref(),
            Some(bytes.as_slice())
        );
        let cached = CachedFs::new(fs.clone());
        cached.read_ahead(|queue| {
            queue(path.to_owned());
            for loader in [Loader::Bytes, Loader::Binary, Loader::Base64] {
                let source = load(&cached, path, loader, &opts, None).expect("load");
                assert!(source.js.contains(base64), "{}", source.js);
            }
            let source = load(&cached, path, Loader::DataUrl, &opts, None).expect("load");
            assert!(
                source.js.contains(&format!(";base64,{base64}")),
                "{}",
                source.js
            );
            let source = load(&cached, path, Loader::File, &opts, None).expect("load");
            assert_eq!(
                source.asset.expect("asset").bytes.as_ref(),
                bytes.as_slice()
            );
        });
    }
    std::fs::remove_dir_all(dir).expect("cleanup");
}

#[test]
fn attributed_bytes_import_preserves_invalid_utf8() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("attributed-loader-bytes");
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(
        dir.join("entry.js"),
        "import bytes from './data.bin' with { type: 'bytes' }; console.log(bytes);",
    )
    .expect("write entry");
    std::fs::write(dir.join("data.bin"), [0xff]).expect("write bytes");
    let opts = BuildOptions {
        working_dir: dir.to_str().expect("path").replace('\\', "/"),
        entry_points: vec![EntryPoint {
            path: "entry.js".into(),
            name: None,
        }],
        bundle: true,
        write: false,
        threads: 2,
        ..BuildOptions::default()
    };
    let result = BuildContext::new(opts, Arc::new(RealFs::new()))
        .expect("options")
        .build();
    assert!(!result.has_errors(), "{:?}", result.messages);
    let output = result.outputs.iter().map(|o| o.text()).collect::<String>();
    assert!(
        output.contains("Uint8Array.fromBase64(\"/w==\")"),
        "{output}"
    );
    std::fs::remove_dir_all(dir).expect("cleanup");
}
