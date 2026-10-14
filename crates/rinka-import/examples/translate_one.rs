//! Translates one effect shader pair and prints WGSL or the error.
//! `cargo run -p rinka-import --example translate_one -- ASSETS_DIR VERT FRAG [COMBO=V…]`

use std::collections::BTreeMap;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let (assets, vert, frag) = (&args[1], &args[2], &args[3]);
    let vs = std::fs::read_to_string(vert)?;
    let fs = std::fs::read_to_string(frag)?;
    let notes = rinka_import::shader::annotations(&vs, &fs);
    let mut combos: BTreeMap<String, i64> = notes
        .combos
        .iter()
        .map(|c| (c.name.clone(), c.default))
        .collect();
    for kv in &args[4..] {
        let (k, v) = kv.split_once('=').expect("COMBO=VALUE");
        combos.insert(k.to_owned(), v.parse()?);
    }
    let root = std::path::PathBuf::from(assets).join("shaders");
    let include = move |n: &str| std::fs::read_to_string(root.join(n)).ok();
    match rinka_import::shader::translate(&vs, &fs, &combos, &include) {
        Ok(t) => println!("{}\n----\n{}\n{:#?}", t.vertex, t.fragment, t.uniforms),
        Err(e) => println!("{e}"),
    }
    Ok(())
}
