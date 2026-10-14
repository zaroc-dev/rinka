//! Checks against locally installed Wallpaper Engine content. Ignored by
//! default; run with `cargo test -p rinka-import -- --ignored` after setting
//! `RINKA_WORKSHOP` (the `431960` workshop directory) and optionally
//! `RINKA_WE_ASSETS` (the install's `assets` directory).

use std::path::PathBuf;

use rinka_import::{pkg::Package, tex};

fn workshop() -> Option<PathBuf> {
    std::env::var_os("RINKA_WORKSHOP").map(PathBuf::from)
}

#[test]
#[ignore = "needs local Wallpaper Engine content"]
fn every_packaged_texture_decodes() {
    let Some(dir) = workshop() else { return };
    let (mut ok, mut animated, mut videos) = (0, 0, 0);
    let mut failures = Vec::new();
    for item in std::fs::read_dir(dir).unwrap().flatten() {
        let pkg_path = item.path().join("scene.pkg");
        let Ok(data) = std::fs::read(&pkg_path) else {
            continue;
        };
        let pkg = Package::parse(data).unwrap_or_else(|e| panic!("{}: {e}", pkg_path.display()));
        for name in pkg.names().filter(|n| n.ends_with(".tex")) {
            match tex::decode(pkg.get(name).unwrap()) {
                Ok(t) => {
                    ok += 1;
                    animated += usize::from(t.animated());
                    videos += usize::from(t.video.is_some());
                    if t.video.is_none() {
                        let img = &t.images[0];
                        if !t.animated() {
                            assert_eq!((img.width(), img.height()), (t.width, t.height), "{name}");
                        }
                    }
                }
                Err(e) => failures.push(format!(
                    "{}/{name}: {e}",
                    item.file_name().to_string_lossy()
                )),
            }
        }
    }
    eprintln!(
        "decoded {ok} textures ({animated} animated, {videos} video); {} failed",
        failures.len()
    );
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
#[ignore = "needs local Wallpaper Engine content"]
fn every_builtin_texture_decodes() {
    let Some(dir) = std::env::var_os("RINKA_WE_ASSETS").map(PathBuf::from) else {
        return;
    };
    let mut stack = vec![dir];
    let (mut ok, mut failures) = (0, Vec::new());
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "tex") {
                match tex::decode(&std::fs::read(&p).unwrap()) {
                    Ok(_) => ok += 1,
                    Err(err) => failures.push(format!("{}: {err}", p.display())),
                }
            }
        }
    }
    eprintln!("decoded {ok} built-in textures; {} failed", failures.len());
    assert!(failures.is_empty(), "{failures:#?}");
}

fn assets() -> Option<PathBuf> {
    std::env::var_os("RINKA_WE_ASSETS").map(PathBuf::from)
}

/// Translates every effect shader pair (built-in and packaged) with default
/// combos and prints a failure summary. Fails only on panics; the pass rate
/// is the measurement.
#[test]
#[ignore = "needs local Wallpaper Engine content"]
fn effect_shader_translation_coverage() {
    use rinka_import::shader;
    use std::collections::BTreeMap;
    let Some(assets) = assets() else { return };
    let include_root = assets.join("shaders");
    let include = move |name: &str| std::fs::read_to_string(include_root.join(name)).ok();

    // (label, vertex source, fragment source)
    let mut pairs: Vec<(String, String, String)> = Vec::new();
    for effect in std::fs::read_dir(assets.join("effects")).unwrap().flatten() {
        let dir = effect.path().join("shaders/effects");
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "frag") {
                let v = p.with_extension("vert");
                if let (Ok(vs), Ok(fs)) = (std::fs::read_to_string(&v), std::fs::read_to_string(&p))
                {
                    pairs.push((
                        format!("builtin {}", p.file_stem().unwrap().to_string_lossy()),
                        vs,
                        fs,
                    ));
                }
            }
        }
    }
    if let Some(ws) = workshop() {
        for item in std::fs::read_dir(ws).unwrap().flatten() {
            let Ok(data) = std::fs::read(item.path().join("scene.pkg")) else {
                continue;
            };
            let pkg = Package::parse(data).unwrap();
            let names: Vec<String> = pkg
                .names()
                .filter(|n| n.ends_with(".frag"))
                .map(str::to_owned)
                .collect();
            for f in names {
                let v = f.trim_end_matches(".frag").to_owned() + ".vert";
                let (Some(fs), Some(vs)) = (pkg.get(&f), pkg.get(&v)) else {
                    continue;
                };
                pairs.push((
                    format!("{} {f}", item.file_name().to_string_lossy()),
                    String::from_utf8_lossy(vs).into_owned(),
                    String::from_utf8_lossy(fs).into_owned(),
                ));
            }
        }
    }

    let mut ok = 0;
    let mut failures: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (label, vs, fs) in &pairs {
        let notes = shader::annotations(vs, fs);
        let combos: BTreeMap<String, i64> = notes
            .combos
            .iter()
            .map(|c| (c.name.clone(), c.default))
            .collect();
        match shader::translate(vs, fs, &combos, &include) {
            Ok(_) => ok += 1,
            Err(e) => {
                let msg = e.to_string();
                let key: String = msg.lines().next().unwrap_or("").chars().take(90).collect();
                failures.entry(key).or_default().push(label.clone());
            }
        }
    }
    eprintln!("translated {ok} of {} effect shader pairs", pairs.len());
    for (k, v) in &failures {
        eprintln!("{:>3} × {k}\n      e.g. {}", v.len(), v[0]);
    }
}
