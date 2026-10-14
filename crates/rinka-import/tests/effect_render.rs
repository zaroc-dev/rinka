//! Renders a scene whose layer runs a translated Wallpaper Engine-style
//! effect, checking the whole path: GLSL dialect → WGSL → engine passes.

use std::collections::{BTreeMap, HashMap};

use rinka_engine::image::{ImageData, PixelFormat};
use rinka_engine::project::{
    Content, Effect, EffectPass, Fit, Layer, LayerBlend, SceneColorSpace, SceneContent, ShaderInfo,
    TextureInfo, UniformInfo,
};
use rinka_engine::{GpuScene, Project, Renderer, SceneAssets, Srgb8};

const VERT: &str = r#"
uniform mat4 g_ModelViewProjectionMatrix;
attribute vec3 a_Position;
attribute vec2 a_TexCoord;
varying vec2 v_TexCoord;
void main() {
    gl_Position = mul(vec4(a_Position, 1.0), g_ModelViewProjectionMatrix);
    v_TexCoord = a_TexCoord;
}
"#;

// Inverts the layer where the mask is white, scaled by a material value.
const FRAG: &str = r#"
varying vec2 v_TexCoord;
uniform sampler2D g_Texture0; // {"material":"framebuffer","hidden":true}
uniform sampler2D g_Texture1; // {"material":"mask","default":"util/white"}
uniform float g_Amount; // {"material":"amount","default":1}
uniform float g_Time;
void main() {
    vec4 c = texSample2D(g_Texture0, v_TexCoord);
    float m = texSample2D(g_Texture1, v_TexCoord).r * g_Amount;
    gl_FragColor = vec4(mix(c.rgb, 1 - c.rgb, m), c.a);
}
"#;

fn solid(w: u32, h: u32, px: [u8; 4]) -> ImageData {
    ImageData {
        width: w,
        height: h,
        format: PixelFormat::Rgba8Raw,
        pixels: px.repeat((w * h) as usize),
        source_size: (w, h),
    }
}

#[test]
fn translated_effect_runs_on_a_layer() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let Ok(adapter) = pollster::block_on(instance.request_adapter(&Default::default())) else {
        eprintln!("skipping: no GPU adapter");
        return;
    };
    let (device, queue) = pollster::block_on(rinka_engine::gpu::request_device(&adapter)).unwrap();
    let renderer = Renderer::new(
        &device,
        &queue,
        rinka_engine::capture::CAPTURE_FORMAT.add_srgb_suffix(),
    );

    let t = rinka_import::shader::translate(VERT, FRAG, &BTreeMap::new(), &|_| None)
        .expect("translates");
    let info = ShaderInfo {
        vertex: "shaders/invert.vert.wgsl".into(),
        fragment: "shaders/invert.frag.wgsl".into(),
        uniforms: t
            .uniforms
            .iter()
            .map(|u| UniformInfo {
                name: u.name.clone(),
                offset: u.offset,
                ty: u.ty.clone(),
                size: u.size,
            })
            .collect(),
        uniform_size: t.uniform_size,
        textures: t.textures.clone(),
    };
    // Left half masked, right half not.
    let mut mask = solid(2, 1, [0, 0, 0, 255]);
    mask.pixels[..4].copy_from_slice(&[255, 0, 0, 255]);
    let content = SceneContent {
        width: 4,
        height: 2,
        fit: Fit::Stretch,
        clear_color: [0.0, 0.0, 0.0],
        color_space: SceneColorSpace::Gamma,
        layers: vec![Layer {
            id: "1".into(),
            name: "layer".into(),
            visible: true,
            texture: "textures/base.png".into(),
            origin: [2.0, 1.0],
            size: [4.0, 2.0],
            scale: [1.0, 1.0],
            angle: 0.0,
            color: [1.0; 3],
            alpha: 1.0,
            brightness: 1.0,
            blend: LayerBlend::Translucent,
            effects: vec![Effect {
                id: "2".into(),
                name: "invert".into(),
                visible: true,
                passes: vec![EffectPass {
                    shader: "invert".into(),
                    textures: vec![None, Some("textures/mask.png".into())],
                    values: BTreeMap::from([("g_Amount".into(), vec![1.0])]),
                }],
            }],
            particles: None,
        }],
        textures: BTreeMap::from([
            (
                "textures/base.png".into(),
                TextureInfo {
                    clamp: true,
                    nearest: true,
                },
            ),
            (
                "textures/mask.png".into(),
                TextureInfo {
                    clamp: true,
                    nearest: true,
                },
            ),
        ]),
        shaders: BTreeMap::from([("invert".into(), info)]),
    };
    let assets = SceneAssets {
        textures: HashMap::from([
            ("textures/base.png".into(), solid(4, 2, [40, 100, 200, 255])),
            ("textures/mask.png".into(), mask),
        ]),
        shaders: HashMap::from([
            ("shaders/invert.vert.wgsl".into(), t.vertex.clone()),
            ("shaders/invert.frag.wgsl".into(), t.fragment.clone()),
        ]),
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
        rinka_engine::capture::capture(&device, &queue, &renderer, &mut target, (4, 2), 0.0);
    let px: Vec<[u8; 4]> = pixels.as_chunks::<4>().0.to_vec();
    // Gamma-space: values pass through unchanged. Masked half inverted.
    assert_eq!(px[0], [215, 155, 55, 255], "{px:?}");
    assert_eq!(px[1], [215, 155, 55, 255], "{px:?}");
    assert_eq!(px[3], [40, 100, 200, 255], "{px:?}");
    assert_eq!(px[7], [40, 100, 200, 255], "{px:?}");
}
