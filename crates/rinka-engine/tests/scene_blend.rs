//! Color-blended layers on the GPU: the layer is mixed with what is below
//! by its formula, and layers above it still compose normally.

use std::collections::{BTreeMap, HashMap};

use rinka_engine::image::{ImageData, PixelFormat};
use rinka_engine::project::{
    ColorBlend, Content, Fit, Layer, LayerBlend, SceneColorSpace, SceneContent, TextureInfo,
};
use rinka_engine::{GpuScene, Project, Renderer, SceneAssets, Srgb8};

const BASE: [u8; 4] = [200, 100, 50, 255];
const GREY: [u8; 4] = [128, 128, 128, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];
const GREEN: [u8; 4] = [30, 200, 30, 255];

fn solid(px: [u8; 4]) -> ImageData {
    ImageData {
        width: 1,
        height: 1,
        format: PixelFormat::Rgba8Raw,
        pixels: px.to_vec(),
        source_size: (1, 1),
    }
}

/// A layer of `texture` over canvas columns `x0..x1` of a 4×1 canvas.
fn layer(id: &str, texture: &str, x0: f32, x1: f32) -> Layer {
    Layer {
        id: id.into(),
        name: id.into(),
        visible: true,
        texture: texture.into(),
        origin: [(x0 + x1) / 2.0, 0.5],
        size: [x1 - x0, 1.0],
        scale: [1.0, 1.0],
        angle: 0.0,
        color: [1.0; 3],
        alpha: 1.0,
        brightness: 1.0,
        blend: LayerBlend::Translucent,
        color_blend: None,
        effects: Vec::new(),
        particles: None,
        mesh: None,
        attach: None,
    }
}

fn render(layers: Vec<Layer>) -> Option<Vec<[u8; 4]>> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let Ok(adapter) = pollster::block_on(instance.request_adapter(&Default::default())) else {
        eprintln!("skipping: no GPU adapter");
        return None;
    };
    let (device, queue) = pollster::block_on(rinka_engine::gpu::request_device(&adapter)).unwrap();
    let renderer = Renderer::new(
        &device,
        &queue,
        rinka_engine::capture::CAPTURE_FORMAT.add_srgb_suffix(),
    );
    let colors = [
        ("textures/base.png", BASE),
        ("textures/grey.png", GREY),
        ("textures/blue.png", BLUE),
        ("textures/green.png", GREEN),
    ];
    let info = TextureInfo {
        clamp: true,
        nearest: true,
    };
    let content = SceneContent {
        width: 4,
        height: 1,
        fit: Fit::Stretch,
        clear_color: [0.0; 3],
        color_space: SceneColorSpace::Gamma,
        layers,
        textures: colors
            .iter()
            .map(|(p, _)| ((*p).into(), info.clone()))
            .collect(),
        shaders: BTreeMap::new(),
    };
    let assets = SceneAssets {
        textures: colors
            .iter()
            .map(|(p, c)| ((*p).into(), solid(*c)))
            .collect(),
        shaders: HashMap::new(),
        meshes: HashMap::new(),
    };
    let scene = GpuScene::new(
        &device,
        &queue,
        &renderer,
        &content,
        &assets,
        wgpu::TextureFormat::Rgba8Unorm,
    )
    .expect("scene builds");
    let mut project = Project::solid_color("t", Srgb8::BLACK);
    project.content = Content::Scene(Box::new(content));
    let mut target = renderer.create_target(&device, &project, None);
    target.set_scene(Some(scene));
    let pixels =
        rinka_engine::capture::capture(&device, &queue, &renderer, &mut target, (4, 1), 0.0);
    Some(pixels.as_chunks::<4>().0.to_vec())
}

fn close(a: [u8; 4], b: [u8; 4]) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 1)
}

#[test]
fn multiply_mixes_with_below_and_layers_above_compose() {
    let multiply = Layer {
        color_blend: Some(ColorBlend::Multiply),
        ..layer("2", "textures/grey.png", 2.0, 4.0)
    };
    let Some(px) = render(vec![
        layer("1", "textures/base.png", 0.0, 4.0),
        multiply,
        layer("3", "textures/green.png", 0.0, 1.0),
    ]) else {
        return;
    };
    // 200·128/255 = 100.4, 100·128/255 = 50.2, 50·128/255 = 25.1.
    let expected = [GREEN, BASE, [100, 50, 25, 255], [100, 50, 25, 255]];
    for (p, e) in px.iter().zip(expected) {
        assert!(close(*p, e), "{px:?}");
    }
}

#[test]
fn alpha_is_the_blend_opacity() {
    // Screen with blue at half alpha: only blue changes,
    // 50 + (255 − 50)·128/255 = 152.9.
    let screen = Layer {
        color_blend: Some(ColorBlend::Screen),
        alpha: 128.0 / 255.0,
        ..layer("2", "textures/blue.png", 0.0, 4.0)
    };
    let Some(px) = render(vec![layer("1", "textures/base.png", 0.0, 4.0), screen]) else {
        return;
    };
    for p in &px {
        assert!(close(*p, [200, 100, 153, 255]), "{px:?}");
    }
}
