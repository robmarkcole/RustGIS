//! Times the UI thread's frames while the wheel zooms a detailed worldview in and out (#1750).
//!
//! `cargo run --release -p rustgis-ui-egui --example zoom_bench -- countries_default.geojson`
//!
//! The file is a worldview's countries (see `worldview_bench` in `rustgis-render`). Each gesture
//! is a run of wheel steps at 60 frames a second; a frame over 33 ms is one the user sees stall.
//! `GPU=1` draws the map with the GPU backend, `INLINE=1` every frame on the UI thread (as the
//! browser does), `PPP=2` at two device pixels a point.
use rustgis_engine::sample::{self, Detail, View, Worldview};
use rustgis_ui_egui::RustGisApp;

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: zoom_bench countries.geojson");
        return;
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{path}: {e}");
            return;
        }
    };
    let wv = View::new(Worldview::DEFAULT, Detail::Original);
    if let Err(e) = sample::install(wv, text) {
        eprintln!("{e}");
        return;
    }
    rustgis_engine::render::prepare_world(wv);
    sample::set_view(wv);
    let mut app = RustGisApp::with_sample();
    // `INLINE=1`: every frame on the UI thread, as in the browser.
    app.map.inline_frames = std::env::var_os("INLINE").is_some();
    if std::env::var_os("GPU").is_some() {
        let setup = egui_wgpu::WgpuSetup::CreateNew(egui_wgpu::WgpuSetupCreateNew::without_display_handle());
        let instance = pollster::block_on(setup.new_instance());
        match pollster::block_on(egui_wgpu::RenderState::create(
            &egui_wgpu::WgpuConfiguration { wgpu_setup: setup, ..Default::default() },
            &instance,
            None,
            egui_wgpu::RendererOptions::PREDICTABLE,
        )) {
            Ok(rs) => {
                app.set_render_state(Some(rs));
                if let Err(e) = app.run("ui.map.gpu", serde_json::json!({"on": true})) {
                    eprintln!("{e}");
                }
            }
            Err(e) => eprintln!("no GPU: {e}"),
        }
    }
    let mut h = egui_kittest::Harness::builder()
        .with_size(egui::vec2(1480.0, 900.0))
        .with_pixels_per_point(std::env::var("PPP").ok().and_then(|v| v.parse().ok()).unwrap_or(1.0))
        .with_step_dt(1.0 / 60.0)
        .with_max_steps(100_000)
        .build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            app,
        );
    let settle = |h: &mut egui_kittest::Harness<'_, RustGisApp>| {
        // Past the settle delay, then until the worker's exact frame has landed.
        for _ in 0..30 {
            h.step();
        }
        let end = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while h.state().map.drawing() && std::time::Instant::now() < end {
            std::thread::sleep(std::time::Duration::from_millis(2));
            h.step();
        }
        for _ in 0..5 {
            h.step();
        }
        // A gesture timed from a map still drawing wouldn't start where the others do.
        if h.state().map.drawing() {
            eprintln!("the map did not settle in 30 s");
            std::process::exit(1);
        }
    };
    settle(&mut h);
    let at = egui::pos2(800.0, 520.0);
    h.event(egui::Event::PointerMoved(at));
    h.step();
    let gesture = |h: &mut egui_kittest::Harness<'_, RustGisApp>, name: &str, dy: f32, steps: usize| {
        let mut times = Vec::new();
        for _ in 0..steps {
            h.event(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, dy),
                phase: egui::TouchPhase::Move,
                modifiers: egui::Modifiers::NONE,
                source: egui::MouseWheelSource::Wheel,
            });
            let t = std::time::Instant::now();
            h.step();
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            times.push(ms);
            // A real frame lasts 16 ms: the worker gets that long, as it would on screen.
            if ms < 16.0 {
                std::thread::sleep(std::time::Duration::from_secs_f64((16.0 - ms) / 1000.0));
            }
        }
        let slow = times.iter().filter(|t| **t > 33.0).count();
        let max = times.iter().copied().fold(0.0, f64::max);
        let total: f64 = times.iter().sum();
        println!("{name:9}: {steps} frames, {slow:3} over 33 ms, worst {max:6.1} ms, UI thread busy {total:7.0} ms");
        println!("    {}", times.iter().map(|t| format!("{t:.0}")).collect::<Vec<_>>().join(" "));
        settle(h);
    };
    // In to a continent and back out, twice: the second pass finds every level built.
    for pass in ["cold", "warm"] {
        gesture(&mut h, &format!("in  {pass}"), 20.0, 60);
        gesture(&mut h, &format!("out {pass}"), -20.0, 60);
    }
}
