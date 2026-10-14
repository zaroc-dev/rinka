//! Writes a texture from a scene package as PNG, for inspection.
//! `cargo run -p rinka-import --example dump_tex -- SCENE.PKG NAME OUT.PNG`

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let [_, pkg, name, out] = &args[..] else {
        anyhow::bail!("usage: dump_tex SCENE.PKG NAME OUT.PNG");
    };
    let pkg = rinka_import::pkg::Package::parse(std::fs::read(pkg)?)?;
    let data = pkg
        .get(name)
        .ok_or_else(|| anyhow::anyhow!("{name} not in package"))?;
    let tex = rinka_import::tex::decode(data)?;
    println!(
        "{:?} flags={:#x} image {}x{} texture {}x{} images={} frames={}",
        tex.format,
        tex.flags,
        tex.width,
        tex.height,
        tex.texture_width,
        tex.texture_height,
        tex.images.len(),
        tex.frames.len()
    );
    tex.images[0].save(out)?;
    Ok(())
}
