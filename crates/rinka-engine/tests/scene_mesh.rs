//! Mesh layers on the GPU: the mesh, not the layer's quad, decides which
//! texels land where, and skinned meshes follow their animation.

use std::collections::{BTreeMap, HashMap};

use rinka_engine::image::{ImageData, PixelFormat};
use rinka_engine::mesh::{self, Animation, Attachment, Bone, Mesh, MeshAnimation, PlayMode};
use rinka_engine::project::{
    Content, Fit, Layer, LayerAttach, LayerBlend, LayerMesh, SceneColorSpace, SceneContent,
    TextureInfo,
};
use rinka_engine::{GpuScene, Project, Renderer, SceneAssets, Srgb8};

const RED: [u8; 4] = [200, 30, 30, 255];
const BLUE: [u8; 4] = [30, 30, 200, 255];
const BLACK: [u8; 4] = [0, 0, 0, 255];
const GREEN: [u8; 4] = [30, 200, 30, 255];

/// A 4×2 texture, red on the left, blue on the right.
fn texture() -> ImageData {
    let mut pixels = Vec::new();
    for _ in 0..2 {
        for x in 0..4 {
            pixels.extend_from_slice(if x < 2 { &RED } else { &BLUE });
        }
    }
    ImageData {
        width: 4,
        height: 2,
        format: PixelFormat::Rgba8Raw,
        pixels,
        source_size: (4, 2),
    }
}

/// The texture's left half drawn over the layer's right half.
fn mesh() -> Mesh {
    Mesh {
        format: mesh::FORMAT.into(),
        version: mesh::VERSION,
        positions: vec![0.0, -1.0, 2.0, -1.0, 0.0, 1.0, 2.0, 1.0],
        uvs: vec![0.0, 0.0, 0.5, 0.0, 0.0, 1.0, 0.5, 1.0],
        indices: vec![0, 1, 2, 2, 1, 3],
        joints: Vec::new(),
        weights: Vec::new(),
        bones: Vec::new(),
        animations: Vec::new(),
        attachments: Vec::new(),
    }
}

/// [`mesh`] on one bone whose animation ends 2 px to the left.
fn skinned() -> Mesh {
    let frame = |x: f32| [x, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0];
    let mut identity = [0.0; 16];
    for i in 0..4 {
        identity[i * 5] = 1.0;
    }
    Mesh {
        joints: vec![0; 16],
        weights: [1.0, 0.0, 0.0, 0.0].repeat(4),
        bones: vec![Bone {
            parent: None,
            matrix: identity,
        }],
        animations: vec![Animation {
            id: 1,
            name: "slide".into(),
            fps: 1.0,
            length: 1,
            mode: PlayMode::Single,
            tracks: vec![[frame(0.0), frame(-2.0)].concat()],
        }],
        ..mesh()
    }
}

fn render(mesh: Mesh, animations: Vec<MeshAnimation>, time: f64) -> Option<Vec<[u8; 4]>> {
    render_with(mesh, animations, true, Vec::new(), time)
}

/// Renders the mesh layer (shown or not) with `extra` layers above it,
/// which use `textures/green.png`.
fn render_with(
    mesh: Mesh,
    animations: Vec<MeshAnimation>,
    show_mesh: bool,
    extra: Vec<Layer>,
    time: f64,
) -> Option<Vec<[u8; 4]>> {
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
    let mut layers = vec![Layer {
        id: "1".into(),
        name: "layer".into(),
        visible: show_mesh,
        texture: "textures/base.png".into(),
        origin: [2.0, 1.0],
        size: [4.0, 2.0],
        scale: [1.0, 1.0],
        angle: 0.0,
        color: [1.0; 3],
        alpha: 1.0,
        brightness: 1.0,
        blend: LayerBlend::Translucent,
        color_blend: None,
        effects: Vec::new(),
        particles: None,
        mesh: Some(LayerMesh {
            source: "meshes/m.json".into(),
            animations,
        }),
        attach: None,
    }];
    layers.extend(extra);
    let info = TextureInfo {
        clamp: true,
        nearest: true,
    };
    let content = SceneContent {
        width: 4,
        height: 2,
        fit: Fit::Stretch,
        clear_color: [0.0; 3],
        color_space: SceneColorSpace::Gamma,
        layers,
        textures: BTreeMap::from([
            ("textures/base.png".into(), info.clone()),
            ("textures/green.png".into(), info),
        ]),
        shaders: BTreeMap::new(),
    };
    let green = ImageData {
        width: 1,
        height: 1,
        format: PixelFormat::Rgba8Raw,
        pixels: GREEN.to_vec(),
        source_size: (1, 1),
    };
    let assets = SceneAssets {
        textures: HashMap::from([
            ("textures/base.png".into(), texture()),
            ("textures/green.png".into(), green),
        ]),
        shaders: HashMap::new(),
        meshes: HashMap::from([("meshes/m.json".into(), mesh)]),
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
        rinka_engine::capture::capture(&device, &queue, &renderer, &mut target, (4, 2), time);
    Some(pixels.as_chunks::<4>().0.to_vec())
}

#[test]
fn mesh_maps_texture_halves() {
    let Some(px) = render(mesh(), Vec::new(), 0.0) else {
        return;
    };
    for row in px.chunks(4) {
        assert_eq!(row, [BLACK, BLACK, RED, RED], "{px:?}");
    }
}

#[test]
fn skinned_mesh_follows_its_animation() {
    let slide = MeshAnimation {
        animation: 1,
        rate: 1.0,
        blend: 1.0,
        visible: true,
    };
    // Bind pose without animation; the held last frame with it.
    let Some(still) = render(skinned(), Vec::new(), 5.0) else {
        return;
    };
    assert_eq!(&still[..4], [BLACK, BLACK, RED, RED], "{still:?}");
    let moved = render(skinned(), vec![slide], 5.0).unwrap();
    for row in moved.chunks(4) {
        assert_eq!(row, [RED, RED, BLACK, BLACK], "{moved:?}");
    }
}

#[test]
fn attached_layer_follows_its_bone() {
    // A point 1 px right of the bone; a 2×2 layer centered on it.
    let mut pin = [0.0; 16];
    for i in 0..4 {
        pin[i * 5] = 1.0;
    }
    pin[12] = 1.0;
    let rigged = Mesh {
        attachments: vec![Attachment {
            name: "pin".into(),
            bone: 0,
            matrix: pin,
        }],
        ..skinned()
    };
    let attached = Layer {
        id: "2".into(),
        name: "attached".into(),
        visible: true,
        texture: "textures/green.png".into(),
        origin: [0.0, 0.0],
        size: [2.0, 2.0],
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
        attach: Some(LayerAttach {
            layer: "1".into(),
            attachment: "pin".into(),
        }),
    };
    let slide = MeshAnimation {
        animation: 1,
        rate: 1.0,
        blend: 1.0,
        visible: true,
    };
    // Bind pose: the point is at canvas x = 3, so columns 2 and 3.
    let Some(still) = render_with(
        rigged.clone(),
        Vec::new(),
        false,
        vec![attached.clone()],
        5.0,
    ) else {
        return;
    };
    for row in still.chunks(4) {
        assert_eq!(row, [BLACK, BLACK, GREEN, GREEN], "{still:?}");
    }
    // The bone ends 2 px left, and the layer with it.
    let moved = render_with(rigged, vec![slide], false, vec![attached], 5.0).unwrap();
    for row in moved.chunks(4) {
        assert_eq!(row, [GREEN, GREEN, BLACK, BLACK], "{moved:?}");
    }
}

#[test]
fn mesh_assets_round_trip() {
    let m = skinned();
    assert_eq!(Mesh::from_json(&m.to_json()).unwrap(), m);
}
