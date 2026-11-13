//! The render thread of the Mac: mpv draws each frame into a GL texture
//! that the window shows.

use super::*;

#[cfg(target_os = "macos")]
pub(super) struct Renderer {
    ctx: *mut sys::mpv_render_context,
    gl: GlRenderer,
    /// The size the frames are rendered at, the source size they are of,
    /// and the size the video area asks for since when (see `frame_size`).
    size: (u32, u32),
    src: (u32, u32),
    asked: Option<((u32, u32), Instant)>,
    /// For the pacing trace: when mpv asked for the frame in work, when
    /// the swap was last reported to mpv, and mpv's target time of the
    /// frame.
    asked_ns: u64,
    swap_ns: u64,
    target: i64,
    /// The frame mpv has now was not drawn, because the window drew
    /// nothing: it is drawn once the window draws again (`render_loop`).
    unseen: bool,
}

#[cfg(target_os = "macos")]
impl Drop for Renderer {
    fn drop(&mut self) {
        // mpv frees its GL objects here, so the context in `gl` must outlive it.
        unsafe { sys::mpv_render_context_free(self.ctx) };
    }
}

#[cfg(target_os = "macos")]
pub(super) unsafe extern "C" fn on_render_update(ctx: *mut c_void) {
    let signal = unsafe { &*(ctx as *const RenderSignal) };
    if !signal.needed.swap(true, Ordering::AcqRel) {
        signal.asked_ns.store(clock_ns(), Ordering::Release);
    }
    signal.wake();
}


// ----- render thread ---------------------------------------------------------

/// Renders the frames mpv asks for, and reports each refresh of the display
/// to mpv (see `pacing.rs`). It calls only `mpv_render_*` functions, never
/// the core: the sizes it needs come through `Shared`. The display link
/// and mpv's render context live and die here.
#[cfg(target_os = "macos")]
pub(super) fn render_loop(mpv_handle: usize, signal: &'static RenderSignal, ready: mpsc::Sender<Result<()>>) {
    let shared = &signal.shared;
    let mut renderer = match create_renderer(mpv_handle as *mut sys::mpv_handle, signal) {
        Ok(renderer) => renderer,
        Err(err) => {
            let _ = ready.send(Err(err));
            return;
        }
    };
    // The clock of the display: at each tick the swap is reported to mpv,
    // which then hands over the frame for the next refresh.
    let on_tick = Box::new(move || {
        signal.swap_due.store(true, Ordering::Release);
        signal.wake();
    });
    match DisplayClock::start(on_tick) {
        Ok(clock) => {
            let state: &'static pacing::ClockState = clock.state();
            shared.clock_state.store(ptr::from_ref(state).cast_mut(), Ordering::Release);
            *shared.clock.lock().unwrap() = Some(clock);
        }
        // Without the clock the frames keep the audio timing.
        Err(err) => log::warn!("no display link, audio timing for the frames: {err:#}"),
    }
    let display_pacing = shared.clock_state().is_some();
    if display_pacing {
        // The phase loop starts from the model with each worker, and
        // hears of each draw's presented time while this thread lives.
        pacing::phase::reset();
        gpui_apple::present_trace::set_feedback(Some(pacing::phase::on_presented));
    }
    let _ = ready.send(Ok(()));

    while !signal.quit.load(Ordering::Acquire) {
        // What CoreVideo and the GL driver autorelease in this turn goes
        // at its end, not when the thread ends.
        let _pool = crate::macos::Pool::new();
        // The display refreshed: mpv hears of it first, so it can hand
        // over the frame for the next refresh at once.
        if signal.swap_due.swap(false, Ordering::AcqRel) {
            renderer.swap_ns = clock_ns();
            unsafe { sys::mpv_render_context_report_swap(renderer.ctx) };
        }

        // Video frame. A frame in display sync is rendered at once and the
        // call does not wait; one with audio timing waits in the render
        // call for its time.
        //
        // When the window draws again and the frame mpv has was never
        // drawn (`Renderer::unseen`), it is drawn now. mpv asks for no
        // frame while it is paused, so without this the picture of before
        // the window stopped drawing stayed up after a seek or a pause
        // that came meanwhile.
        let watching = || pacing::ui_watching(shared.ui_seen_ns.load(Ordering::Acquire), clock_ns());
        let redraw = signal.redraw.swap(false, Ordering::AcqRel) || (renderer.unseen && watching());
        if signal.needed.swap(false, Ordering::AcqRel) || redraw {
            renderer.asked_ns = signal.asked_ns.load(Ordering::Acquire);
            let flags = unsafe { sys::mpv_render_context_update(renderer.ctx) };
            if flags & (sys::mpv_render_update_flag_MPV_RENDER_UPDATE_FRAME as u64) == 0 {
                // No new frame: with the size just known, the frame mpv has
                // is drawn once more.
                if redraw && let Err(err) = render_frame(&mut renderer, shared, false, false) {
                    log::debug!("redraw skipped: {err}");
                }
            } else {
                let info = next_frame_info(&renderer);
                renderer.target = info.target_time;
                let synced =
                    info.flags & sys::mpv_render_frame_info_flag_MPV_RENDER_FRAME_INFO_BLOCK_VSYNC as u64 != 0;
                let repeat = info.flags & sys::mpv_render_frame_info_flag_MPV_RENDER_FRAME_INFO_REPEAT as u64 != 0
                    && info.flags & sys::mpv_render_frame_info_flag_MPV_RENDER_FRAME_INFO_REDRAW as u64 == 0;
                // The same picture again, or nobody draws: mpv counts the
                // frame as rendered and keeps its timing, the surface stays.
                let watching = watching();
                let skip = display_pacing && !pacing::draws(repeat, watching, renderer.unseen);
                if skip {
                    if watching {
                        pacing::count_repeat();
                    } else {
                        pacing::count_unseen();
                        // A repeat of a frame that was drawn changes nothing.
                        renderer.unseen |= !repeat;
                    }
                }
                if let Err(err) = render_frame(&mut renderer, shared, skip, synced) {
                    log::debug!("render skipped: {err}");
                }
            }
        }

        // Sleep until mpv asks or the display ticks.
        let (lock, cvar) = &signal.wake;
        let mut flag = lock.lock().unwrap();
        if !*flag && !signal.needed.load(Ordering::Acquire) {
            let (guard, _) = cvar.wait_timeout(flag, IDLE_WAIT).unwrap();
            flag = guard;
        }
        *flag = false;
        crate::perf::count_loop(crate::perf::Loop::Render);
    }
    // The clock stops first; mpv's render context goes before the core.
    if display_pacing {
        gpui_apple::present_trace::set_feedback(None);
    }
    shared.clock_state.store(ptr::null_mut(), Ordering::Release);
    *shared.clock.lock().unwrap() = None;
    drop(renderer);
}

#[cfg(target_os = "macos")]
pub(super) fn create_renderer(mpv_handle: *mut sys::mpv_handle, signal: &'static RenderSignal) -> Result<Renderer> {
    // The GL context becomes current on this thread; mpv renders with it.
    let gl = GlRenderer::new()?;
    let mut init = sys::mpv_opengl_init_params {
        get_proc_address: Some(video_surface::get_proc_address),
        get_proc_address_ctx: ptr::null_mut(),
    };
    let mut params = [
        sys::mpv_render_param {
            type_: sys::mpv_render_param_type_MPV_RENDER_PARAM_API_TYPE,
            data: sys::MPV_RENDER_API_TYPE_OPENGL.as_ptr() as *mut c_void,
        },
        sys::mpv_render_param {
            type_: sys::mpv_render_param_type_MPV_RENDER_PARAM_OPENGL_INIT_PARAMS,
            data: &mut init as *mut sys::mpv_opengl_init_params as *mut c_void,
        },
        sys::mpv_render_param {
            type_: sys::mpv_render_param_type_MPV_RENDER_PARAM_INVALID,
            data: ptr::null_mut(),
        },
    ];
    let mut ctx: *mut sys::mpv_render_context = ptr::null_mut();
    let code = unsafe { sys::mpv_render_context_create(&mut ctx, mpv_handle, params.as_mut_ptr()) };
    if code < 0 || ctx.is_null() {
        return Err(anyhow!("mpv_render_context_create failed ({code})"));
    }
    unsafe {
        sys::mpv_render_context_set_update_callback(
            ctx,
            Some(on_render_update),
            signal as *const RenderSignal as *mut c_void,
        );
    }
    Ok(Renderer { ctx, gl, size: (0, 0), src: (0, 0), asked: None, asked_ns: 0, swap_ns: 0, target: 0, unseen: false })
}

/// A new size of the video area counts once it has held for this long:
/// in a resize of the window the area changes at every frame, and each
/// change makes the pool of surfaces anew (`GlRenderer::resize`).
pub(super) const SIZE_SETTLE: Duration = Duration::from_millis(150);

/// The size to render the picture at: the one it is shown at, so mpv's
/// scaler (lanczos) does the upscale and the UI copies the frame one to
/// one, instead of stretching a frame of the source size. The video area
/// in device pixels bounds it, a picture smaller than that keeps its
/// aspect. A new file renders at its size at once; a change of the area
/// after `SIZE_SETTLE`, and until then the UI stretches the frame.
#[cfg(target_os = "macos")]
pub(super) fn frame_size(renderer: &mut Renderer, src: (u32, u32), target: (u32, u32)) -> (u32, u32) {
    let scale = (target.0 as f64 / src.0 as f64).min(target.1 as f64 / src.1 as f64);
    let even = |v: f64| ((v as u32).max(2) / 2) * 2;
    let wanted = (even(src.0 as f64 * scale), even(src.1 as f64 * scale));
    if wanted == renderer.size {
        renderer.asked = None;
    } else {
        let now = Instant::now();
        let held = match renderer.asked {
            Some((size, since)) if size == wanted => now.duration_since(since),
            _ => {
                renderer.asked = Some((wanted, now));
                Duration::ZERO
            }
        };
        if renderer.size == (0, 0) || src != renderer.src || held >= SIZE_SETTLE {
            renderer.size = wanted;
            renderer.asked = None;
        }
    }
    renderer.src = src;
    renderer.size
}

/// What mpv says about the frame it asks to render.
#[cfg(target_os = "macos")]
pub(super) fn next_frame_info(renderer: &Renderer) -> sys::mpv_render_frame_info {
    let mut info = sys::mpv_render_frame_info { flags: 0, target_time: 0 };
    let param = sys::mpv_render_param {
        type_: sys::mpv_render_param_type_MPV_RENDER_PARAM_NEXT_FRAME_INFO,
        data: &mut info as *mut sys::mpv_render_frame_info as *mut c_void,
    };
    unsafe { sys::mpv_render_context_get_info(renderer.ctx, param) };
    info
}

/// The bands the colours of the picture are told in, and the least time
/// between two looks at a frame. Under the time of two frames of a film
/// (83.3 ms at 24 a second) by a margin, so each second frame is looked at
/// and a frame that comes a little early does not move the look to the
/// third: 12 looks a second for a film, 10 to 15 for other rates.
pub const COLOUR_BANDS: usize = 14;
pub(super) const COLOURS_EVERY_NS: u64 = 75_000_000;

/// Renders the current frame at the size of the UI's video area (see
/// `frame_size`), then publishes its GPU surface.
/// With `skip` mpv takes the frame as rendered and nothing is drawn.
/// `synced` says the frame is in display sync: made for the next refresh.
#[cfg(target_os = "macos")]
pub(super) fn render_frame(renderer: &mut Renderer, shared: &Shared, skip: bool, synced: bool) -> Result<()> {
    // The time of the tick a frame in display sync is made after. A frame
    // with audio timing waits in the render call for its time instead and
    // gets no tick.
    let (tick, tick_idx, vsync) = match shared.clock_state() {
        Some(state) if synced => (
            state.tick_ns.load(Ordering::Acquire),
            state.ticks.load(Ordering::Acquire),
            state.vsync_ns.load(Ordering::Acquire),
        ),
        _ => (0, 0, 0),
    };
    if skip {
        let mut one: i32 = 1;
        let mut params = [
            sys::mpv_render_param {
                type_: sys::mpv_render_param_type_MPV_RENDER_PARAM_SKIP_RENDERING,
                data: &mut one as *mut i32 as *mut c_void,
            },
            sys::mpv_render_param {
                type_: sys::mpv_render_param_type_MPV_RENDER_PARAM_INVALID,
                data: ptr::null_mut(),
            },
        ];
        let code = unsafe { sys::mpv_render_context_render(renderer.ctx, params.as_mut_ptr()) };
        if code < 0 {
            return Err(anyhow!("mpv_render_context_render (skip) failed ({code})"));
        }
        return Ok(());
    }
    let src_w = shared.dwidth.load(Ordering::Relaxed);
    let src_h = shared.dheight.load(Ordering::Relaxed);
    if src_w == 0 || src_h == 0 {
        // The size is on its way from the worker (its event can come after
        // mpv's first frame): this frame is not drawn, and the worker asks
        // for it again when the size is there (`RenderSignal::redraw`).
        pacing::count_no_size();
        return render_frame(renderer, shared, true, synced);
    }
    let target_w = shared.target_w.load(Ordering::Relaxed).max(16);
    let target_h = shared.target_h.load(Ordering::Relaxed).max(16);
    let (w, h) = frame_size(renderer, (src_w, src_h), (target_w, target_h));

    let mut fbo = sys::mpv_opengl_fbo {
        fbo: renderer.gl.begin(w, h)?,
        w: w as i32,
        h: h as i32,
        internal_format: 0,
    };
    let mut params = [
        sys::mpv_render_param {
            type_: sys::mpv_render_param_type_MPV_RENDER_PARAM_OPENGL_FBO,
            data: &mut fbo as *mut sys::mpv_opengl_fbo as *mut c_void,
        },
        sys::mpv_render_param {
            type_: sys::mpv_render_param_type_MPV_RENDER_PARAM_INVALID,
            data: ptr::null_mut(),
        },
    ];
    let code = unsafe { sys::mpv_render_context_render(renderer.ctx, params.as_mut_ptr()) };
    if code < 0 {
        return Err(anyhow!("mpv_render_context_render failed ({code})"));
    }
    renderer.unseen = false;
    let frame = renderer.gl.finish();
    let seq = shared.publish(Some(frame), tick, vsync);
    pacing::count_rendered(synced);
    // From mpv's request to the finished frame. mpv asks early and the
    // render call waits for the time of the frame, so this is about the
    // time of one frame. The UI measures its own wait from here.
    let asked = renderer.asked_ns;
    let now = clock_ns();
    pacing::trace::rendered(pacing::trace::FrameRec {
        seq,
        tick_idx,
        tick_ns: tick,
        vsync_ns: vsync,
        swap_ns: renderer.swap_ns,
        asked_ns: asked,
        done_ns: now,
        target: renderer.target,
        ..Default::default()
    });
    crate::perf::video_frame_rendered(now.saturating_sub(asked) as f64 / 1e6);
    shared.frame_done_ns.store(now, Ordering::Release);
    let _ = shared.frames.0.try_send(());
    // The colours, after the frame is on its way to the UI: the read
    // waits for the GPU, and before 2026-11-10 it held the frame back for
    // that time, twelve times a second.
    if shared.colours_on.load(Ordering::Relaxed)
        && now.saturating_sub(shared.colours_ns.load(Ordering::Relaxed)) >= COLOURS_EVERY_NS
    {
        shared.colours_ns.store(now, Ordering::Relaxed);
        // The first row of the surface is the top of the picture.
        let bands = renderer.gl.colours(COLOUR_BANDS);
        let mut colours = shared.colours.lock().unwrap();
        *colours = (colours.0 + 1, bands);
    }
    Ok(())
}
