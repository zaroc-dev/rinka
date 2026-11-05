//! Video textures in scenes: a frame's Y'CbCr planes become the encoded RGB
//! the layer samples, with the matrix and range the project names.

use std::collections::{BTreeMap, HashMap};

use rinka_engine::project::{
    Content, Fit, Layer, LayerBlend, SceneColorSpace, SceneContent, TextureInfo, VideoTextureInfo,
};
use rinka_engine::{
    ColorMatrix, ColorRange, GpuScene, Project, Renderer, SceneAssets, Srgb8, YuvFrame,
};

const VIDEO: &str = "textures/clip.mp4";

fn render(matrix: ColorMatrix, full_range: bool, yuv: [u8; 3]) -> Option<(Vec<[u8; 4]>, bool)> {
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
    let content = SceneContent {
        width: 4,
        height: 2,
        fit: Fit::Stretch,
        clear_color: [0.0; 3],
        color_space: SceneColorSpace::Gamma,
        layers: vec![Layer {
            id: "1".into(),
            name: "video".into(),
            visible: true,
            texture: VIDEO.into(),
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
            mesh: None,
            attach: None,
        }],
        textures: BTreeMap::from([(
            VIDEO.into(),
            TextureInfo {
                clamp: true,
                nearest: false,
                video: Some(VideoTextureInfo {
                    width: 4,
                    height: 2,
                    matrix,
                    full_range,
                }),
            },
        )]),
        shaders: BTreeMap::new(),
        sounds: Vec::new(),
    };
    let assets = SceneAssets {
        textures: HashMap::new(),
        shaders: HashMap::new(),
        meshes: HashMap::new(),
    };
    let mut scene = GpuScene::new(
        &device,
        &queue,
        &renderer,
        &content,
        &assets,
        wgpu::TextureFormat::Rgba8Unorm,
    )
    .expect("scene builds");
    let animated = scene.animated();
    assert_eq!(scene.videos().collect::<Vec<_>>(), [VIDEO]);
    let (y, u, v) = ([yuv[0]; 8], [yuv[1]; 2], [yuv[2]; 2]);
    let frame = YuvFrame {
        width: 4,
        height: 2,
        chroma_width: 2,
        chroma_height: 1,
        y: &y,
        y_stride: 4,
        u: &u,
        u_stride: 2,
        v: &v,
        v_stride: 2,
        interleaved: false,
        // The project's matrix and range apply, not the frame's.
        matrix: ColorMatrix::Bt2020Ncl,
        range: ColorRange::Full,
    };
    scene.upload_video(&device, &renderer, &queue, VIDEO, &frame);
    let mut project = Project::solid_color("t", Srgb8::BLACK);
    project.content = Content::Scene(Box::new(content));
    let mut target = renderer.create_target(&device, &project, None);
    target.set_scene(Some(scene));
    let pixels =
        rinka_engine::capture::capture(&device, &queue, &renderer, &mut target, (4, 2), 0.0);
    Some((pixels.as_chunks::<4>().0.to_vec(), animated))
}

#[test]
fn frames_convert_with_the_projects_matrix_and_range() {
    // The same codes as the video wallpaper fixtures, with independent
    // BT.601/709 equations.
    for (matrix, full) in [(ColorMatrix::Bt709, false), (ColorMatrix::Bt601, true)] {
        let Some((px, animated)) = render(matrix, full, [97, 138, 90]) else {
            return;
        };
        assert!(animated);
        let (kr, kb) = match matrix {
            ColorMatrix::Bt601 => (0.299_f64, 0.114_f64),
            _ => (0.2126, 0.0722),
        };
        let (y, cb, cr) = if full {
            (97.0 / 255.0, 10.0 / 255.0, -38.0 / 255.0)
        } else {
            (81.0 / 219.0, 10.0 / 224.0, -38.0 / 224.0)
        };
        let kg = 1.0 - kr - kb;
        let expected = [
            y + 2.0 * (1.0 - kr) * cr,
            y - 2.0 * kb * (1.0 - kb) / kg * cb - 2.0 * kr * (1.0 - kr) / kg * cr,
            y + 2.0 * (1.0 - kb) * cb,
        ]
        .map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8);
        for p in &px {
            for k in 0..3 {
                assert!(
                    p[k].abs_diff(expected[k]) <= 1,
                    "{matrix:?}: {p:?} vs {expected:?}"
                );
            }
        }
    }
}
