use super::*;

/// Finding P7 (verify-playback): the picture of a frame, its number
/// and its tick are read as one, and the number never goes backwards,
/// also while the render thread publishes and the UI acknowledges the
/// end at the same time. On 0.1.2 the three were separate stores: a
/// reader could pair the tick of one frame with the picture of the
/// next, and after `acknowledge_end` the tick of the last frame stayed
/// with no picture.
#[test]
fn a_frame_is_read_with_its_own_tick_and_its_number_never_goes_back() {
    let player = Player::default();
    let shared = player.shared.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let render = {
        let (shared, stop) = (shared.clone(), stop.clone());
        thread::spawn(move || {
            let picture = VideoFrame::blank(16, 16);
            // A frame in display sync has a tick; the tick it is
            // published with is its own identity here.
            let mut tick = 1_000;
            while !stop.load(Ordering::Relaxed) {
                shared.publish(Some(picture.clone()), tick, 0);
                tick += 1_000;
            }
        })
    };
    let ack = {
        let (player, stop) = (player.clone(), stop.clone());
        thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                player.acknowledge_end();
            }
        })
    };
    let mut last_seq = 0;
    let deadline = Instant::now() + Duration::from_millis(500);
    let mut reads = 0u64;
    while Instant::now() < deadline {
        let before = player.frame_seq();
        let published = player.frame();
        let after = player.frame_seq();
        // The snapshot is one: a picture has a tick, the end has none.
        assert_eq!(
            published.frame.is_some(),
            published.tick_ns != 0,
            "seq {}: picture {} with tick {}",
            published.seq,
            published.frame.is_some(),
            published.tick_ns
        );
        // The numbers go forward, in the atomic and in the snapshot.
        assert!(published.seq >= last_seq, "seq went back: {} after {last_seq}", published.seq);
        assert!(
            before <= published.seq && published.seq <= after,
            "atomic {before}..{after} around snapshot {}",
            published.seq
        );
        last_seq = published.seq;
        reads += 1;
    }
    stop.store(true, Ordering::Relaxed);
    render.join().unwrap();
    ack.join().unwrap();
    assert!(reads > 1000 && player.frame_seq() > 1000, "too few rounds: {reads} reads, seq {}", player.frame_seq());
}

#[test]
fn parses_track_list_json() {
    let json = r#"[{"id":1,"type":"video","selected":true},
        {"id":1,"type":"audio","lang":"eng","codec":"aac","selected":true,"default":true},
        {"id":2,"type":"sub","title":"English SDH","lang":"eng","codec":"subrip","selected":false}]"#;
    let tracks: Vec<Track> = serde_json::from_str(json).unwrap();
    assert_eq!(tracks.len(), 3);
    assert_eq!(tracks[1].label(), "ENG · AAC");
    assert_eq!(tracks[2].label(), "English SDH · ENG · SUBRIP");
}

/// Plays a synthetic clip through the embedded core, checks frames arrive
/// at the size of the video area (twice the clip), pauses, seeks, and
/// stops. Skipped when libmpv or ffmpeg are unavailable.
#[test]
fn embedded_playback_roundtrip() {
    let Some(_one) = real_player_turn() else { return };
    let Some(media) = test_clip("bloom-embedded-test.mp4") else {
        return;
    };
    let client = Client::new("http://127.0.0.1:9", "test-device").with_session("token", "user");
    let player = Player::default();
    let _closer = Closer(player.clone());
    player.set_target_size(640, 480);
    // The clip has a tone: a test must never be heard.
    player.set_muted(true);
    player.play(PlayRequest {
        client,
        item_id: "test".into(),
        url: media.display().to_string(),
        title: "Embedded test".into(),
        start_secs: 0.,
        paused: false,
        token: 0,
        play_session_id: None,
        play_method: String::new(),
        transcoding: false,
        media_source_id: String::new(),
        subtitles: Vec::new(),
    });
    roundtrip(&player);
}

/// A 30 s clip with a tone; `None` when ffmpeg is not there.
fn test_clip(name: &str) -> Option<std::path::PathBuf> {
    let media = std::env::temp_dir().join(name);
    let encoded = std::process::Command::new("ffmpeg")
        .args([
            "-y",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            // Red top half, blue bottom half: checks orientation and chroma.
            "color=c=red:s=320x240:r=10:d=30,drawbox=x=0:y=120:w=320:h=120:color=blue:t=fill",
        ])
        .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=30"])
        .args([
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-shortest",
        ])
        .arg(&media)
        .status();
    // No ffmpeg is a skip (`dev/test` refuses to start without it); an
    // ffmpeg that runs and fails must not let the test pass unplayed.
    match encoded {
        Ok(status) => assert!(status.success(), "ffmpeg could not make the test clip: {status}"),
        Err(_) => {
            eprintln!("ffmpeg not available; skipping");
            return None;
        }
    }
    Some(media)
}

fn roundtrip(player: &Player) {
    let wait = |what: &str, mut ok: Box<dyn FnMut() -> bool>| {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !ok() {
            let status = player.status();
            assert!(status.error.is_none(), "{what}: {status:?}");
            assert!(Instant::now() < deadline, "{what} timed out: {status:?}");
            thread::sleep(Duration::from_millis(50));
        }
    };
    let p = player.clone();
    wait(
        "playing",
        Box::new(move || p.status().state == PlayState::Playing),
    );
    // Every caller plays a clip with a tone: the roundtrip must be silent.
    assert_eq!(player.probe("mute").as_deref(), Some("yes"), "a test plays with sound");
    let p = player.clone();
    wait("first frame", Box::new(move || p.frame().frame.is_some()));
    let frame = player.frame().frame;
    let frame = frame.unwrap();
    // The video area is 640 x 480: mpv scales the 320 x 240 clip up.
    assert_eq!(frame.width(), 640);
    assert_eq!(frame.height(), 480);
    // Let the GPU finish, then read pixels (bytes are B, G, R, A).
    thread::sleep(Duration::from_millis(300));
    let buffer = player.frame().frame.unwrap().buffer();
    buffer.lock_base_address(core_video::pixel_buffer::kCVPixelBufferLock_ReadOnly);
    let stride = buffer.get_bytes_per_row();
    let pixel = |x: usize, y: usize| unsafe {
        let base = buffer.get_base_address() as *const u8;
        let at = base.add(y * stride + x * 4);
        (*at.add(2), *at.add(1), *at)
    };
    let (top, bottom) = (pixel(320, 80), pixel(320, 400));
    buffer.unlock_base_address(core_video::pixel_buffer::kCVPixelBufferLock_ReadOnly);
    assert!(top.0 > 200 && top.2 < 60, "top must be red, got RGB {top:?}");
    assert!(bottom.2 > 200 && bottom.0 < 60, "bottom must be blue, got RGB {bottom:?}");
    let p = player.clone();
    wait(
        "tracks",
        Box::new(move || p.status().tracks.iter().any(|t| t.kind == TrackKind::Audio)),
    );
    player.toggle_pause();
    let p = player.clone();
    wait("paused", Box::new(move || p.status().paused));
    player.seek_absolute(20.0);
    let p = player.clone();
    wait("seeked", Box::new(move || p.status().position >= 19.0));
    player.stop();
    let p = player.clone();
    wait(
        "ended",
        Box::new(move || p.status().state == PlayState::Ended),
    );
    player.acknowledge_end();
    assert_eq!(player.status().state, PlayState::Idle);
}

/// Ends a server process with the test, also when the test fails.
struct Kill(std::process::Child);

impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A server whose certificate no root signs is refused: libmpv checks
/// nothing by itself (`tls_roots_file`). The server is openssl's own,
/// with a certificate it makes for the test. Skipped without openssl.
#[test]
fn refuses_a_self_signed_server() {
    let Some(_one) = real_player_turn() else { return };
    let Some(media) = test_clip("bloom-embedded-tls-test.mp4") else {
        return;
    };
    let dir = std::env::temp_dir().join("bloom-tls-test");
    std::fs::create_dir_all(&dir).unwrap();
    let (cert, key) = (dir.join("cert.pem"), dir.join("key.pem"));
    let made = std::process::Command::new("openssl")
        .args(["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-subj", "/CN=127.0.0.1"])
        .arg("-keyout")
        .arg(&key)
        .arg("-out")
        .arg(&cert)
        .stderr(std::process::Stdio::null())
        .status();
    if !made.map(|s| s.success()).unwrap_or(false) {
        eprintln!("openssl not available; skipping");
        return;
    }
    std::fs::copy(&media, dir.join("clip.mp4")).unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    // The server logs `FILE:clip.mp4` when a client asks for the file:
    // a client that refuses the certificate never does.
    let log = dir.join("server.log");
    let _server = Kill(
        std::process::Command::new("openssl")
            .args(["s_server", "-WWW", "-accept"])
            .arg(port.to_string())
            .arg("-cert")
            .arg(&cert)
            .arg("-key")
            .arg(&key)
            .current_dir(&dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::fs::File::create(&log).unwrap())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline, "openssl s_server did not start");
        thread::sleep(Duration::from_millis(50));
    }

    let client = Client::new("http://127.0.0.1:9", "test-device").with_session("token", "user");
    let player = Player::default();
    let _closer = Closer(player.clone());
    player.set_target_size(320, 240);
    player.play(PlayRequest {
        client,
        item_id: "test".into(),
        url: format!("https://127.0.0.1:{port}/clip.mp4"),
        title: "TLS test".into(),
        start_secs: 0.,
        paused: false,
        token: 3,
        play_session_id: None,
        play_method: String::new(),
        transcoding: false,
        media_source_id: String::new(),
        subtitles: Vec::new(),
    });
    let deadline = Instant::now() + Duration::from_secs(15);
    while player.status().state != PlayState::Ended {
        assert!(player.frame().frame.is_none(), "a frame of the refused server");
        assert!(Instant::now() < deadline, "not refused: {:?}", player.status());
        thread::sleep(Duration::from_millis(50));
    }
    let status = player.status();
    assert!(status.error.is_some(), "{status:?}");
    let events = player.events();
    let failed = std::iter::from_fn(|| events.try_recv().ok())
        .any(|event| event == PlayerEvent::LoadFailed { token: 3 });
    assert!(failed, "no LoadFailed event: {status:?}");
    thread::sleep(Duration::from_millis(200));
    let served = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(!served.contains("FILE:"), "the file was asked for: {served}");
}

/// The next event that fits, within ten seconds.
fn expect(player: &Player, what: &str, mut fits: impl FnMut(&PlayerEvent) -> bool) -> PlayerEvent {
    let events = player.events();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match events.try_recv() {
            Ok(event) if fits(&event) => return event,
            Ok(_) => {}
            Err(_) => {
                let status = player.status();
                assert!(status.error.is_none(), "{what}: {status:?}");
                assert!(Instant::now() < deadline, "{what} timed out: {status:?}");
                thread::sleep(Duration::from_millis(2));
            }
        }
    }
}

/// Position measurements over a time, and the speed they show.
fn measured_speed(player: &Player, over: Duration) -> (usize, f64) {
    let events = player.events();
    while events.try_recv().is_ok() {}
    thread::sleep(over);
    let samples: Vec<Sample> = std::iter::from_fn(|| events.try_recv().ok())
        .filter_map(|event| match event {
            PlayerEvent::Position(sample) => Some(sample),
            _ => None,
        })
        .collect();
    let (first, last) = (samples.first().unwrap(), samples.last().unwrap());
    let speed = (last.position - first.position) / (last.at - first.at).as_secs_f64();
    (samples.len(), speed)
}

/// How far the position is from where a steady clock would put it, after
/// each change of the speed. Prints the numbers; run with --nocapture.
#[test]
#[ignore = "a measurement, not a check"]
fn speed_change_position_error() {
    // The turn of a test with the real player: a second player at the
    // same time would spoil these numbers and the other test's.
    let Some(_one) = real_player_turn() else { return };
    let Some(media) = test_clip("bloom-embedded-speed-test.mp4") else {
        return;
    };
    let client = Client::new("http://127.0.0.1:9", "test-device").with_session("token", "user");
    let player = Player::default();
    let _closer = Closer(player.clone());
    player.set_target_size(320, 240);
    player.play(PlayRequest {
        client,
        item_id: "test".into(),
        url: media.display().to_string(),
        title: "Speed test".into(),
        start_secs: 2.,
        paused: false,
        token: 1,
        play_session_id: None,
        play_method: String::new(),
        transcoding: false,
        media_source_id: String::new(),
        subtitles: Vec::new(),
    });
    player.set_muted(true);
    expect(&player, "loaded", |e| *e == PlayerEvent::Loaded { token: 1 });
    player.follow(true);
    thread::sleep(Duration::from_millis(1500));
    let events = player.events();
    let latest = |events: &async_channel::Receiver<PlayerEvent>| {
        let mut last = None;
        while let Ok(event) = events.try_recv() {
            if let PlayerEvent::Position(sample) = event {
                last = Some(sample);
            }
        }
        last
    };
    thread::sleep(Duration::from_millis(300));
    let start = latest(&events).unwrap();
    // Position a steady clock would give, from the speeds set so far.
    let mut expected = start.position;
    let mut since = start.at;
    let mut speed = 1.0;
    for (factor, hold_ms) in [(0.97, 2000), (1.0, 1500), (1.03, 2000), (1.0, 1500), (0.95, 1500), (1.0, 1500)] {
        let now = Instant::now();
        expected += (now - since).as_secs_f64() * speed;
        since = now;
        speed = factor;
        player.set_sync_speed(factor);
        thread::sleep(Duration::from_millis(hold_ms));
        let sample = latest(&events).unwrap();
        let at_sample = expected + (sample.at - since).as_secs_f64() * speed;
        let status = player.status();
        let video = status.position
            - (expected + (status.position_at.unwrap() - since).as_secs_f64() * speed);
        eprintln!(
            "speed {factor:.2} for {hold_ms} ms: audio position off by {:+.1} ms, video by {:+.1} ms",
            (sample.position - at_sample) * 1000.,
            video * 1000.
        );
    }
    player.stop();
}

/// What group playback needs of the player: a paused load at a position,
/// a start at a set time, exact seeks that report when they are done,
/// position measurements, a speed correction, and the end of the item.
#[test]
fn precise_control_for_group_playback() {
    let Some(_one) = real_player_turn() else { return };
    let Some(media) = test_clip("bloom-embedded-sync-test.mp4") else {
        return;
    };
    let client = Client::new("http://127.0.0.1:9", "test-device").with_session("token", "user");
    let player = Player::default();
    let _closer = Closer(player.clone());
    player.set_target_size(320, 240);
    let request = |start_secs: f64, paused: bool, token: u64| PlayRequest {
        client: client.clone(),
        item_id: "test".into(),
        url: media.display().to_string(),
        title: "Sync test".into(),
        start_secs,
        paused,
        token,
        play_session_id: None,
        play_method: String::new(),
        transcoding: false,
        media_source_id: String::new(),
        subtitles: Vec::new(),
    };

    // A paused load at a position with a fraction.
    player.play(request(5.5, true, 7));
    player.set_muted(true);
    expect(&player, "loaded", |e| *e == PlayerEvent::Loaded { token: 7 });
    let status = player.status();
    assert!(status.paused, "{status:?}");
    assert!((status.position - 5.5).abs() < 0.1, "{status:?}");
    thread::sleep(Duration::from_millis(300));
    assert!((player.status().position - 5.5).abs() < 0.1, "moved while paused");

    // A start at a set time.
    player.schedule(Some(Scheduled {
        at: Instant::now() + Duration::from_millis(400),
        action: ScheduledAction::Unpause,
    }));
    thread::sleep(Duration::from_millis(250));
    assert!(player.status().paused, "started before its time");
    let fired = expect(&player, "scheduled start", |e| {
        matches!(e, PlayerEvent::ScheduledFired { .. })
    });
    let PlayerEvent::ScheduledFired { late } = fired else { unreachable!() };
    eprintln!("scheduled start ran {late:?} after its time");
    assert!(late < Duration::from_millis(20), "start was {late:?} late");
    let p = player.clone();
    let deadline = Instant::now() + Duration::from_secs(5);
    while p.status().paused || p.status().position < 5.6 {
        assert!(Instant::now() < deadline, "did not start: {:?}", p.status());
        thread::sleep(Duration::from_millis(10));
    }

    // Position measurements, and a correction of the speed.
    player.follow(true);
    thread::sleep(Duration::from_millis(500));
    let (count, speed) = measured_speed(&player, Duration::from_millis(1500));
    assert!(count >= 10, "{count} measurements");
    eprintln!("{count} measurements in 1.5 s, speed {speed:.4}");
    assert!((speed - 1.0).abs() < 0.03, "speed {speed}");
    player.set_sync_speed(1.08);
    thread::sleep(Duration::from_millis(500));
    let (_, speed) = measured_speed(&player, Duration::from_millis(1500));
    eprintln!("speed with a factor of 1.08: {speed:.4}");
    assert!((speed - 1.08).abs() < 0.03, "corrected speed {speed}");
    player.set_sync_speed(1.0);
    player.follow(false);

    // An exact seek that parks the player and says when it is done.
    player.seek_exact(20.25, true, 9);
    expect(&player, "settled", |e| *e == PlayerEvent::Settled { token: 9 });
    let status = player.status();
    assert!(status.paused, "{status:?}");
    assert!((status.position - 20.25).abs() < 0.1, "{status:?}");

    // A pause at a set time that goes to a position.
    player.set_paused(false);
    player.set_paused(false);
    thread::sleep(Duration::from_millis(300));
    assert!(!player.status().paused);
    player.schedule(Some(Scheduled {
        at: Instant::now() + Duration::from_millis(200),
        action: ScheduledAction::PauseThenSeek(12.0),
    }));
    expect(&player, "scheduled pause", |e| matches!(e, PlayerEvent::ScheduledFired { .. }));
    let p = player.clone();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !(p.status().paused && (p.status().position - 12.0).abs() < 0.1) {
        assert!(Instant::now() < deadline, "did not park: {:?}", p.status());
        thread::sleep(Duration::from_millis(10));
    }

    // A new item starts to play though the one before was paused.
    player.play(request(0., false, 11));
    player.set_muted(true);
    expect(&player, "second load", |e| *e == PlayerEvent::Loaded { token: 11 });
    thread::sleep(Duration::from_millis(400));
    let status = player.status();
    assert!(!status.paused && status.position > 0.1, "{status:?}");

    // The end of the item.
    player.seek_exact(29.0, false, 12);
    expect(&player, "ended", |e| *e == PlayerEvent::Ended);
    player.acknowledge_end();
}

// ----- verify-playback: the findings of the review of 2026-10-05 --------

/// Where in `play_prepared` a test can run something once: after the
/// check of the load count, or after the worker was started and before
/// the load is sent to it.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum At {
    Checked,
    Started,
}

static HOOK: Mutex<Option<(At, Box<dyn Fn() + Send>)>> = Mutex::new(None);

fn set_hook(at: At, hook: impl Fn() + Send + 'static) {
    *HOOK.lock().unwrap_or_else(|e| e.into_inner()) = Some((at, Box::new(hook)));
}

pub(super) fn hook(at: At) {
    let hook = {
        let mut slot = HOOK.lock().unwrap_or_else(|e| e.into_inner());
        if slot.as_ref().is_some_and(|(wanted, _)| *wanted == at) { slot.take() } else { None }
    };
    if let Some((_, hook)) = hook {
        hook();
    }
}

/// A 30 s clip with no sound: two silent audio tracks and two subtitle
/// tracks (so a choice of the second one can be told from what mpv
/// selects by itself, the first), for a test that must never be heard.
#[test]
fn a_subtitle_file_gets_the_extension_mpv_knows() {
    let at = |end: &str| subtitle_extension(&format!("https://media.example.com/Videos/i/s/Subtitles/3/0/Stream.{end}"));
    assert_eq!(at("pgssub?ApiKey=x"), "sup");
    assert_eq!(at("sup"), "sup");
    assert_eq!(at("vobsub"), "mks");
    assert_eq!(at("mks"), "mks");
    assert_eq!(at("srt"), "srt");
    assert_eq!(at("ASS"), "ass");
    assert_eq!(at("vtt#frag"), "vtt");
    // What the server converts to SubRip, and an address without an extension.
    assert_eq!(at("mov_text"), "srt");
    assert_eq!(subtitle_extension("https://media.example.com/no-extension"), "srt");
    assert_eq!(subtitle_extension("https://a.b/x"), "srt");
}

/// The files of `dev/make-sub-clip` (a PGS and a VobSub, real ones
/// every decoder takes), made once per run; none without ffmpeg.
fn sub_clip() -> Option<std::path::PathBuf> {
    let folder = std::env::temp_dir().join("bloom-test-sub-clip");
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("dev/make-sub-clip");
    let made = std::process::Command::new("python3").arg("-I").arg(script).arg(&folder).status();
    match made {
        Ok(status) if status.success() => Some(folder),
        Ok(status) => panic!("dev/make-sub-clip failed: {status}"),
        Err(_) => {
            eprintln!("python3 not available; skipping");
            None
        }
    }
}

/// A server of files by the end of the path; any other path is a 404.
fn file_server(files: Vec<(&'static str, std::path::PathBuf)>) -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut request = [0u8; 4096];
            let n = std::io::Read::read(&mut stream, &mut request).unwrap_or(0);
            let head = String::from_utf8_lossy(&request[..n]).to_string();
            let path = head.split_whitespace().nth(1).unwrap_or_default().to_string();
            let body = files.iter().find(|(end, _)| path.ends_with(end)).and_then(|(_, file)| std::fs::read(file).ok());
            let answer = match body {
                Some(body) => {
                    let head = format!("HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                    [head.into_bytes(), body].concat()
                }
                None => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
            };
            let _ = std::io::Write::write_all(&mut stream, &answer);
        }
    });
    port
}

/// An image track comes as a file the server names `Stream.pgssub` or
/// `Stream.vobsub`: written with the extension mpv knows, it is a
/// subtitle track of the right codec. A file that starts shown is
/// fetched at the start, an image track that does not waits for the
/// choice (`fetch_subtitle_file`), and a file that fails to come is
/// reported and shifts no id of the others: the ids map by the file.
#[test]
fn image_subtitle_files_are_tracks_of_their_codec_and_map_by_file() {
    let Some(_one) = real_player_turn() else { return };
    let Some(clip) = silent_clip("bloom-test-image-subs.mp4") else { return };
    let Some(subs) = sub_clip() else { return };
    let port = file_server(vec![
        ("/Subtitles/1/0/Stream.pgssub", subs.join("Subtitle Clip (2026).eng.sup")),
        ("/Subtitles/2/0/Stream.vobsub", subs.join("Subtitle Clip (2026).eng.mks")),
    ]);
    let file = |index: i64, format: &str, select: bool, graphical: bool| SubtitleFile {
        index,
        url: format!("http://127.0.0.1:{port}/Videos/i/s/Subtitles/{index}/0/Stream.{format}"),
        title: format!("file {index}"),
        lang: "eng".into(),
        select,
        graphical,
    };
    let player = Player::default();
    let _closer = Closer(player.clone());
    let mut request = request(&clip.to_string_lossy(), "image subtitles", 1);
    // The PGS starts shown; the text file 3 is not on the server; the
    // VobSub is an image track not chosen, so it waits.
    request.subtitles = vec![file(1, "pgssub", true, true), file(3, "srt", false, false), file(2, "vobsub", false, true)];
    player.play(request);
    let wait = |what: &str, done: &dyn Fn(&PlayerStatus) -> bool| {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let status = player.status();
            assert!(status.error.is_none(), "{status:?}");
            if done(&status) {
                return status;
            }
            assert!(Instant::now() < deadline, "{what} did not happen: {status:?}");
            thread::sleep(Duration::from_millis(50));
        }
    };
    let codec = |status: &PlayerStatus, sid: i64| {
        status.tracks.iter().find(|t| t.kind == TrackKind::Sub && t.id == sid).and_then(|t| t.codec.clone()).unwrap_or_default()
    };
    let status = wait("the PGS file", &|s| s.external_sids.iter().any(|(index, _)| *index == 1) && s.pending_subs.is_empty());
    let pgs = status.external_sids.iter().find(|(index, _)| *index == 1).map(|(_, sid)| *sid).unwrap();
    let status = wait("the PGS track", &|s| !codec(s, pgs).is_empty());
    assert_eq!(codec(&status, pgs), "hdmv_pgs_subtitle", "{status:?}");
    assert!(status.tracks.iter().any(|t| t.id == pgs && t.selected), "the PGS starts shown: {status:?}");
    // The lost text file was not to start shown, so it is no failure
    // to report; the VobSub was not asked for.
    assert_eq!(player.take_failed_subtitles(), Vec::<i64>::new());
    assert!(status.external_sids.iter().all(|(index, _)| *index == 1), "{status:?}");
    // The VobSub comes when it is chosen, and gets the next id.
    player.fetch_subtitle_file(file(2, "vobsub", true, true));
    let status = wait("the VobSub file", &|s| s.external_sids.iter().any(|(index, _)| *index == 2));
    let vobsub = status.external_sids.iter().find(|(index, _)| *index == 2).map(|(_, sid)| *sid).unwrap();
    assert_eq!(vobsub, pgs + 1, "{status:?}");
    let status = wait("the VobSub track", &|s| !codec(s, vobsub).is_empty());
    assert_eq!(codec(&status, vobsub), "dvd_subtitle", "{status:?}");
    assert_eq!(status.external_sids, vec![(1, pgs), (2, vobsub)]);
    // A chosen file that does not come is reported, and the map stands.
    player.fetch_subtitle_file(file(3, "srt", true, false));
    let deadline = Instant::now() + Duration::from_secs(10);
    let failed = loop {
        let failed = player.take_failed_subtitles();
        if !failed.is_empty() {
            break failed;
        }
        assert!(Instant::now() < deadline, "the lost file was not reported: {:?}", player.status());
        thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(failed, vec![3]);
    assert_eq!(player.status().external_sids, vec![(1, pgs), (2, vobsub)]);
    player.stop();
}

fn silent_clip(name: &str) -> Option<std::path::PathBuf> {
    let media = std::env::temp_dir().join(name);
    let subs = std::env::temp_dir().join(format!("{name}.srt"));
    std::fs::write(&subs, "1\n00:00:00,000 --> 00:00:30,000\nquiet\n").expect("write the subtitle fixture");
    let encoded = std::process::Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", "color=c=red:s=320x240:r=10:d=30"])
        .args(["-f", "lavfi", "-i", "anullsrc=r=48000:cl=stereo"])
        .args(["-f", "lavfi", "-i", "anullsrc=r=48000:cl=mono"])
        .arg("-i")
        .arg(&subs)
        .args(["-map", "0:v", "-map", "1:a", "-map", "2:a", "-map", "3:s", "-map", "3:s"])
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-c:s", "mov_text", "-t", "30"])
        .arg(&media)
        .status();
    // No ffmpeg is a skip (`dev/test` refuses to start without it); an
    // ffmpeg that runs and fails must not let the test pass unplayed.
    match encoded {
        Ok(status) => assert!(status.success(), "ffmpeg could not make the test clip: {status}"),
        Err(_) => {
            eprintln!("ffmpeg not available; skipping");
            return None;
        }
    }
    Some(media)
}

fn request(url: &str, title: &str, token: u64) -> PlayRequest {
    PlayRequest {
        client: Client::new("http://127.0.0.1:9", "test-device").with_session("token", "user"),
        item_id: "test".into(),
        url: url.to_string(),
        title: title.to_string(),
        start_secs: 0.,
        paused: false,
        token,
        play_session_id: None,
        play_method: String::new(),
        transcoding: false,
        media_source_id: String::new(),
        subtitles: Vec::new(),
    }
}

/// Whether an event comes within `wait`.
fn comes(player: &Player, wait: Duration, mut fits: impl FnMut(&PlayerEvent) -> bool) -> bool {
    let events = player.events();
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        if std::iter::from_fn(|| events.try_recv().ok()).any(|e| fits(&e)) {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    false
}

/// A subtitle file that takes long to come (the server makes the text
/// subtitles of a transcode at the first request, over a minute for a
/// large file) must not hold the worker: the item plays on, a stop
/// takes effect at once, and the subtitle shows when it comes. Before
/// 2026-10-17 mpv fetched the file itself, inside `sub-add`, and the
/// worker stood still for the time it took.
#[test]
fn a_slow_subtitle_file_does_not_hold_the_player() {
    let Some(_one) = real_player_turn() else { return };
    let Some(clip) = silent_clip("bloom-test-slow-sub.mp4") else { return };
    const DELAY: Duration = Duration::from_secs(8);
    // A server that answers a request for the subtitle after `DELAY`.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            thread::spawn(move || {
                let mut request = [0u8; 4096];
                let _ = std::io::Read::read(&mut stream, &mut request);
                thread::sleep(DELAY);
                let body = "1\n00:00:00,000 --> 00:00:30,000\nslow\n";
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = std::io::Write::write_all(&mut stream, format!("{head}{body}").as_bytes());
            });
        }
    });
    let player = Player::default();
    let _closer = Closer(player.clone());
    let mut request = request(&clip.to_string_lossy(), "slow subtitle", 1);
    request.subtitles = vec![SubtitleFile {
        index: 2,
        url: format!("http://127.0.0.1:{port}/Subtitles/2/0/Stream.srt"),
        title: "slow".into(),
        lang: "eng".into(),
        select: true,
        graphical: false,
    }];
    let started = Instant::now();
    player.play(request);
    // The item plays on while the file is on its way.
    let deadline = started + DELAY * 3 / 4;
    loop {
        let status = player.status();
        assert!(status.error.is_none(), "{status:?}");
        if status.state == PlayState::Playing && status.position >= 1. {
            break;
        }
        assert!(Instant::now() < deadline, "the player stood still while the subtitle was fetched: {status:?}");
        thread::sleep(Duration::from_millis(50));
    }
    // The subtitle comes when the server answers.
    let deadline = started + DELAY + Duration::from_secs(10);
    while !player.status().tracks.iter().any(|t| t.kind == TrackKind::Sub && t.title.as_deref() == Some("slow")) {
        assert!(Instant::now() < deadline, "the subtitle never came: {:?}", player.status().tracks);
        thread::sleep(Duration::from_millis(50));
    }
    // The clip has two subtitles of its own: the file is mpv's third.
    let slow = player.status().tracks.into_iter().find(|t| t.title.as_deref() == Some("slow")).unwrap();
    assert!(slow.selected, "the subtitle that starts shown is selected: {slow:?}");
    assert_eq!(slow.id, 3);
    // A stop takes effect at once.
    player.stop();
    let deadline = Instant::now() + Duration::from_secs(3);
    while player.status().state != PlayState::Ended {
        assert!(Instant::now() < deadline, "the stop waited: {:?}", player.status());
        thread::sleep(Duration::from_millis(20));
    }
}

/// Finding P1: a stop while the server is still asked cancels the
/// prepared load, so the answer does not start playback behind a
/// closed player. Here the stop comes before the check of the answer.
#[test]
fn stop_cancels_a_pending_load() {
    let Some(_one) = real_player_turn() else { return };
    let player = Player::default();
    let _closer = Closer(player.clone());
    let generation = player.prepare("Pending", 0., false);
    assert_eq!(player.status().state, PlayState::Starting);
    player.stop();
    let accepted = player.play_prepared(generation, request("/nonexistent/clip.mp4", "Pending", 41));
    assert!(!accepted, "the load prepared before the stop was accepted after it");
    let status = player.status();
    assert_ne!(status.state, PlayState::Starting, "still opening after a stop: {status:?}");
}

/// Finding P1, the race Codex named: the stop comes after the check of
/// `play_prepared` and before the load reaches the worker. The worker
/// refuses the load, answers it, and nothing plays. First with no
/// worker yet (the first item of a launch), then with one.
#[test]
fn stop_between_the_check_and_the_load_cancels_it() {
    let Some(_one) = real_player_turn() else { return };
    let Some(media) = silent_clip("bloom-stop-race-test.mp4") else {
        return;
    };
    let player = Player::default();
    let _closer = Closer(player.clone());
    player.set_target_size(320, 240);
    player.set_muted(true);
    let url = media.display().to_string();

    // No worker yet: the stop finds none, and the worker that then
    // starts refuses the load.
    let generation = player.prepare("First", 0., false);
    let stopper = player.clone();
    set_hook(At::Checked, move || stopper.stop());
    assert!(player.play_prepared(generation, request(&url, "First", 51)), "the check must pass: the stop comes after it");
    assert!(comes(&player, Duration::from_secs(5), |e| *e == PlayerEvent::LoadFailed { token: 51 }), "the cancelled load was not answered");
    assert!(!comes(&player, Duration::from_secs(2), |e| *e == PlayerEvent::Loaded { token: 51 }), "the load played behind the stop");
    let status = player.status();
    assert_eq!(status.state, PlayState::Ended, "{status:?}");

    // The stop reaches the idle worker ahead of the load.
    let generation = player.prepare("Idle", 0., false);
    let stopper = player.clone();
    set_hook(At::Started, move || stopper.stop());
    assert!(player.play_prepared(generation, request(&url, "Idle", 54)));
    assert!(comes(&player, Duration::from_secs(5), |e| *e == PlayerEvent::LoadFailed { token: 54 }), "the cancelled load was not answered");
    assert!(!comes(&player, Duration::from_secs(2), |e| *e == PlayerEvent::Loaded { token: 54 }), "the load played behind the stop");
    let status = player.status();
    assert_eq!(status.state, PlayState::Ended, "{status:?}");

    // A worker exists, an item plays, and the stop races the next load.
    let generation = player.prepare("Second", 0., false);
    assert!(player.play_prepared(generation, request(&url, "Second", 52)));
    expect(&player, "second loaded", |e| *e == PlayerEvent::Loaded { token: 52 });
    let generation = player.prepare("Third", 0., false);
    let stopper = player.clone();
    set_hook(At::Started, move || stopper.stop());
    assert!(player.play_prepared(generation, request(&url, "Third", 53)));
    assert!(comes(&player, Duration::from_secs(5), |e| *e == PlayerEvent::LoadFailed { token: 53 }), "the cancelled load was not answered");
    assert!(!comes(&player, Duration::from_secs(2), |e| *e == PlayerEvent::Loaded { token: 53 }), "the load played behind the stop");
    let status = player.status();
    assert_eq!(status.state, PlayState::Ended, "{status:?}");
    assert_eq!(player.probe("path").as_deref(), Some(""), "a file is still loaded after the stop");
}

/// A load fails at the server while an item still plays (a change of
/// the quality on a bad connection): the status says `Ended`, and the
/// core plays on. The app then closes the player, stops it and takes
/// the end, as the poll of `app.rs` does. Nothing plays after that, and
/// the end of the old item, which comes after the app took the end,
/// does not put `Ended` back into a status at rest.
#[test]
fn a_failed_load_over_a_playing_item_ends_with_the_stop_of_the_app() {
    let Some(_one) = real_player_turn() else { return };
    let Some(media) = silent_clip("bloom-failed-reload-test.mp4") else {
        return;
    };
    let player = Player::default();
    let _closer = Closer(player.clone());
    player.set_target_size(320, 240);
    player.set_muted(true);
    let generation = player.prepare("First", 0., false);
    assert!(player.play_prepared(generation, request(&media.display().to_string(), "First", 61)));
    expect(&player, "first loaded", |e| *e == PlayerEvent::Loaded { token: 61 });
    let generation = player.prepare("Again", 0., false);
    player.fail_load(generation, 62, "the server did not answer", false);
    assert_eq!(player.status().state, PlayState::Ended);
    assert_eq!(player.probe("idle-active").as_deref(), Some("no"), "the item should still play here");
    // What the app does when it closes the player.
    player.stop();
    player.acknowledge_end();
    let end = Instant::now() + Duration::from_secs(5);
    while player.probe("idle-active").as_deref() != Some("yes") && Instant::now() < end {
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(player.probe("idle-active").as_deref(), Some("yes"), "the item plays on behind the closed player");
    // The end of the file has reached the worker by now.
    thread::sleep(Duration::from_millis(200));
    let status = player.status();
    assert_eq!(status.state, PlayState::Idle, "the end of the old item came back: {status:?}");
}

/// Finding P1: a stop and a new start back to back (SyncPlay does
/// this): the new item plays.
#[test]
fn a_start_right_after_a_stop_plays() {
    let Some(_one) = real_player_turn() else { return };
    let Some(media) = silent_clip("bloom-stop-start-test.mp4") else {
        return;
    };
    let player = Player::default();
    let _closer = Closer(player.clone());
    player.set_target_size(320, 240);
    player.set_muted(true);
    let url = media.display().to_string();
    player.play(request(&url, "First", 61));
    expect(&player, "first loaded", |e| *e == PlayerEvent::Loaded { token: 61 });
    player.stop();
    let generation = player.prepare("Second", 0., false);
    assert!(player.play_prepared(generation, request(&url, "Second", 62)));
    expect(&player, "second loaded", |e| *e == PlayerEvent::Loaded { token: 62 });
    assert_eq!(player.status().state, PlayState::Playing);
    player.stop();
}

/// A silent 10 s clip with interlaced frames: what a DVD rip or a TV
/// recording holds.
fn interlaced_clip(name: &str) -> Option<std::path::PathBuf> {
    let media = std::env::temp_dir().join(name);
    let encoded = std::process::Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc2=s=720x576:r=50:d=10"])
        .args(["-f", "lavfi", "-i", "anullsrc=r=48000:cl=stereo"])
        .args(["-vf", "tinterlace=interleave_top,fieldorder=tff", "-flags", "+ilme+ildct"])
        .args(["-c:v", "mpeg2video", "-q:v", "4", "-c:a", "aac", "-shortest", "-t", "10"])
        .arg(&media)
        .status();
    // No ffmpeg is a skip (`dev/test` refuses to start without it); an
    // ffmpeg that runs and fails must not let the test pass unplayed.
    match encoded {
        Ok(status) => assert!(status.success(), "ffmpeg could not make the test clip: {status}"),
        Err(_) => {
            eprintln!("ffmpeg not available; skipping");
            return None;
        }
    }
    Some(media)
}

/// An interlaced file is deinterlaced by itself, and a progressive one
/// is left alone: no filter, no cost.
#[test]
fn an_interlaced_file_is_deinterlaced_and_a_progressive_one_is_not() {
    let Some(_one) = real_player_turn() else { return };
    let (Some(interlaced), Some(progressive)) =
        (interlaced_clip("bloom-interlaced-test.mkv"), silent_clip("bloom-progressive-test.mp4"))
    else {
        return;
    };
    let player = Player::default();
    let _closer = Closer(player.clone());
    player.set_target_size(320, 240);
    player.set_muted(true);
    // The filter comes in with the first frames, a moment after the load.
    let active_within = |want: &str, secs: u64| {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            let got = player.probe("deinterlace-active").unwrap_or_default();
            if got == want || Instant::now() >= deadline {
                return got;
            }
            thread::sleep(Duration::from_millis(50));
        }
    };
    player.play(request(&interlaced.display().to_string(), "Interlaced", 95));
    expect(&player, "interlaced loaded", |e| *e == PlayerEvent::Loaded { token: 95 });
    assert_eq!(player.probe("deinterlace").as_deref(), Some("auto"));
    assert_eq!(active_within("yes", 5), "yes", "an interlaced file plays with its comb lines");
    player.stop();
    player.play(request(&progressive.display().to_string(), "Progressive", 96));
    expect(&player, "progressive loaded", |e| *e == PlayerEvent::Loaded { token: 96 });
    thread::sleep(Duration::from_millis(800));
    assert_eq!(active_within("no", 1), "no", "a progressive file went through the deinterlace filter");
    player.stop();
}

/// Finding P3: the settings the app sends before the first worker
/// exists (mute, volume, speed, subtitle look) and the track choices
/// for the first item must reach the first load.
#[test]
fn first_load_keeps_settings_sent_before_the_worker() {
    let Some(_one) = real_player_turn() else { return };
    let Some(media) = silent_clip("bloom-first-settings-test.mp4") else {
        return;
    };
    let player = Player::default();
    let _closer = Closer(player.clone());
    player.set_target_size(320, 240);
    // As `Bloom::begin` does: prepare (the server is asked), then the
    // settings, then the answer arrives and the worker starts. Each
    // setting at its latest value.
    let generation = player.prepare("Settings test", 0., false);
    player.set_muted(false);
    player.set_muted(true);
    player.set_volume(80.);
    player.set_volume(30.);
    player.set_speed(1.5);
    player.set_property("sub-scale", "1.200");
    player.set_property("sub-scale", "1.500");
    player.set_audio(Some(2));
    player.set_subtitle(Some(2));
    assert!(player.play_prepared(generation, request(&media.display().to_string(), "Settings test", 21)));
    expect(&player, "loaded", |e| *e == PlayerEvent::Loaded { token: 21 });
    let read = |name: &str| player.probe(name).unwrap_or_else(|| format!("no answer for {name}"));
    let number = |name: &str| read(name).parse::<f64>().unwrap_or(f64::NAN);
    let got = (read("mute"), number("volume"), number("speed"), number("sub-scale"), read("aid"), read("sid"));
    assert_eq!(got.0, "yes", "mute was lost: {got:?}");
    assert!((got.1 - 30.).abs() < 0.01, "volume was lost: {got:?}");
    assert!((got.2 - 1.5).abs() < 0.01, "speed was lost: {got:?}");
    assert!((got.3 - 1.5).abs() < 0.01, "subtitle scale was lost: {got:?}");
    assert_eq!(got.4, "2", "the audio track choice was lost: {got:?}");
    assert_eq!(got.5, "2", "the subtitle choice was lost: {got:?}");
    player.stop();
}

/// Finding P3: the track choice made for a first load that was
/// cancelled must not reach the next item; the settings do.
#[test]
fn a_cancelled_first_load_keeps_its_track_choice_to_itself() {
    let Some(_one) = real_player_turn() else { return };
    let Some(media) = silent_clip("bloom-cancelled-tracks-test.mp4") else {
        return;
    };
    let player = Player::default();
    let _closer = Closer(player.clone());
    player.set_target_size(320, 240);
    let first = player.prepare("First", 0., false);
    player.set_muted(true);
    player.set_audio(Some(2));
    player.set_subtitle(Some(2));
    player.stop();
    assert!(!player.play_prepared(first, request(&media.display().to_string(), "First", 71)));
    let second = player.prepare("Second", 0., false);
    assert!(player.play_prepared(second, request(&media.display().to_string(), "Second", 72)));
    expect(&player, "loaded", |e| *e == PlayerEvent::Loaded { token: 72 });
    let read = |name: &str| player.probe(name).unwrap_or_else(|| format!("no answer for {name}"));
    let got = (read("mute"), read("aid"), read("sid"));
    assert_eq!(got.0, "yes", "mute was lost: {got:?}");
    assert_eq!(got.1, "1", "the audio choice of the cancelled load leaked: {got:?}");
    assert_ne!(got.2, "2", "the subtitle choice of the cancelled load leaked: {got:?}");
    player.stop();
}

/// The choice of a track waits for the first worker and then travels
/// inside the load. A stop that comes after the worker started, with
/// the load on its way, cancels the choice with the load: the next
/// item must not get it.
#[test]
fn a_load_cancelled_on_its_way_to_the_worker_keeps_its_track_choice_to_itself() {
    let Some(_one) = real_player_turn() else { return };
    let Some(media) = silent_clip("bloom-cancelled-on-the-way-test.mp4") else {
        return;
    };
    let player = Player::default();
    let _closer = Closer(player.clone());
    player.set_target_size(320, 240);
    let url = media.display().to_string();
    let first = player.prepare("First", 0., false);
    player.set_muted(true);
    player.set_audio(Some(2));
    player.set_subtitle(Some(2));
    let stopper = player.clone();
    set_hook(At::Started, move || stopper.stop());
    assert!(player.play_prepared(first, request(&url, "First", 81)), "the check must pass: the stop comes after it");
    assert!(comes(&player, Duration::from_secs(5), |e| *e == PlayerEvent::LoadFailed { token: 81 }), "the cancelled load was not answered");
    let second = player.prepare("Second", 0., false);
    assert!(player.play_prepared(second, request(&url, "Second", 82)));
    expect(&player, "loaded", |e| *e == PlayerEvent::Loaded { token: 82 });
    let read = |name: &str| player.probe(name).unwrap_or_else(|| format!("no answer for {name}"));
    let got = (read("mute"), read("aid"), read("sid"));
    assert_eq!(got.0, "yes", "mute was lost: {got:?}");
    assert_eq!(got.1, "1", "the audio choice of the cancelled load leaked: {got:?}");
    assert_ne!(got.2, "2", "the subtitle choice of the cancelled load leaked: {got:?}");
    player.stop();
}

/// A transcode that the server prepared, for a load the worker then
/// refuses, is ended on the server: no session of the worker knows it,
/// so nothing else would.
#[test]
fn a_transcode_of_a_load_the_worker_refuses_is_ended_on_the_server() {
    use std::io::{Read, Write};
    let Some(_one) = real_player_turn() else { return };
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (seen_tx, seen_rx) = mpsc::channel::<String>();
    let server = thread::spawn(move || {
        // One request is all the test waits for.
        let Ok((mut socket, _)) = listener.accept() else { return };
        let _ = socket.set_read_timeout(Some(Duration::from_secs(3)));
        let mut buffer = [0u8; 2048];
        let n = socket.read(&mut buffer).unwrap_or(0);
        let head = String::from_utf8_lossy(&buffer[..n]);
        let _ = seen_tx.send(head.lines().next().unwrap_or_default().to_string());
        let _ = socket.write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    });
    let player = Player::default();
    let _closer = Closer(player.clone());
    let generation = player.prepare("Transcode", 0., false);
    let stopper = player.clone();
    set_hook(At::Started, move || stopper.stop());
    let mut req = request("/nonexistent/clip.mp4", "Transcode", 91);
    req.client = Client::new(&format!("http://127.0.0.1:{port}"), "device-1").with_session("token", "user");
    req.play_method = "Transcode".into();
    req.transcoding = true;
    req.play_session_id = Some("session-91".into());
    assert!(player.play_prepared(generation, req));
    assert!(comes(&player, Duration::from_secs(5), |e| *e == PlayerEvent::LoadFailed { token: 91 }), "the refused load was not answered");
    let line = seen_rx.recv_timeout(Duration::from_secs(5)).expect("no request ended the transcode");
    assert!(line.starts_with("DELETE /Videos/ActiveEncodings"), "{line}");
    assert!(line.contains("playSessionId=session-91"), "{line}");
    let _ = server.join();
}

/// Finding P8: a worker that fails before it has a core must still
/// answer the load it was started for, once, as SyncPlay waits for
/// that answer; whether the load reached its channel or not.
#[test]
fn a_worker_that_fails_to_start_reports_the_load_as_failed() {
    let Some(_one) = real_player_turn() else { return };
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            FAIL_INIT.store(false, Ordering::Relaxed);
        }
    }
    let _reset = Reset;
    FAIL_INIT.store(true, Ordering::Relaxed);
    for round in 0..20 {
        let player = Player::default();
        let _closer = Closer(player.clone());
        let token = 31 + round;
        // Every other round the load is sent only after the worker has
        // failed and withdrawn (`play_prepared` answers it); in the
        // other rounds the two race, and the load mostly waits in the
        // channel of the failed worker (the worker answers it).
        if round % 2 == 1 {
            let commands = player.commands.clone();
            set_hook(At::Started, move || {
                let deadline = Instant::now() + Duration::from_secs(3);
                while commands.lock().unwrap().is_some() && Instant::now() < deadline {
                    thread::sleep(Duration::from_millis(1));
                }
            });
        }
        player.play(request("/nonexistent/clip.mp4", "Init failure", token));
        let events = player.events();
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut failed = 0;
        while Instant::now() < deadline {
            failed += std::iter::from_fn(|| events.try_recv().ok())
                .filter(|event| *event == PlayerEvent::LoadFailed { token })
                .count();
            if failed > 0 && player.commands.lock().unwrap().is_none() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        thread::sleep(Duration::from_millis(50));
        failed += std::iter::from_fn(|| events.try_recv().ok())
            .filter(|event| *event == PlayerEvent::LoadFailed { token })
            .count();
        let status = player.status();
        assert_eq!(status.state, PlayState::Ended, "round {round}: {status:?}");
        assert_eq!(failed, 1, "round {round}: LoadFailed {} times for the load of a worker that failed to start: {status:?}", failed);
    }
}

// ----- the review of 2026-11-10 ------------------------------------------

/// A worker with no core, for what it does with its state alone, and
/// the reports it sends.
fn worker_of(generation: u64) -> (Worker, mpsc::Receiver<Report>) {
    let (reports, sent) = mpsc::channel();
    let (tx, _) = mpsc::channel();
    let mut worker = Worker::new(reports, tx);
    worker.generation = generation;
    (worker, sent)
}

fn session_of(item: &str) -> Session {
    Session {
        client: Client::new("http://127.0.0.1:9", "test-device").with_session("token", "user"),
        item_id: item.into(),
        play_session_id: "session".into(),
        play_method: String::new(),
        transcoding: false,
        media_source_id: String::new(),
        last_report: Instant::now(),
        position: 0.,
    }
}

fn events_of(player: &Player) -> Vec<PlayerEvent> {
    let events = player.events();
    std::iter::from_fn(|| events.try_recv().ok()).collect()
}

/// The next load is prepared and waits for the server while the item
/// before still plays. What that item does then (its position, its
/// end) is not of the prepared load: the status stays `Starting`.
/// Before, its end marked the prepared load `Ended` with
/// `reached_end`, and the app went on in the queue or closed the
/// player and cancelled the load.
#[test]
fn the_end_of_the_item_before_does_not_end_a_prepared_load() {
    let player = Player::default();
    let shared = &player.shared;
    let first = player.prepare("First", 0., false);
    let (mut worker, reports) = worker_of(first);
    let mut session = Some(session_of("first"));
    assert!(!handle_event(&Event::PlaybackRestart, shared, &mut session, &mut worker));
    assert_eq!(player.status().state, PlayState::Playing);

    player.prepare("Second", 30., false);
    let moved = Event::PropertyChange { name: "time-pos", change: PropertyData::Double(99.), reply_userdata: 1 };
    assert!(!handle_event(&moved, shared, &mut session, &mut worker));
    assert!(!handle_event(&Event::PlaybackRestart, shared, &mut session, &mut worker));
    assert!(handle_event(&Event::EndFile(mpv_end_file_reason::Eof), shared, &mut session, &mut worker));
    finish(shared, &mut session, &mut worker);
    let status = player.status();
    assert_eq!(
        (status.state, status.title.as_str(), status.position, status.reached_end),
        (PlayState::Starting, "Second", 30., false),
        "the end of the first item reached the status of the second: {status:?}"
    );
    assert_eq!(events_of(&player), vec![], "an end was told for a load that has not played");
    // The server still hears of the stop, at the place of the first item.
    let report = reports.try_recv().expect("no stop report");
    assert!(report.kind == ReportKind::Stopped && report.progress.item_id == "first");
    assert_eq!(report.progress.position_ticks, 99 * TICKS_PER_SECOND);

    // The end of the item the status is of ends it, as before.
    let third = player.prepare("Third", 0., false);
    let (mut worker, _reports) = worker_of(third);
    let mut session = Some(session_of("third"));
    handle_event(&Event::PlaybackRestart, shared, &mut session, &mut worker);
    assert!(handle_event(&Event::EndFile(mpv_end_file_reason::Eof), shared, &mut session, &mut worker));
    finish(shared, &mut session, &mut worker);
    let status = player.status();
    assert!(status.state == PlayState::Ended && status.reached_end, "{status:?}");
    assert_eq!(events_of(&player), vec![PlayerEvent::Ended]);
}

/// A remux is a stream the server makes, as a transcode is, and its
/// reports name it "DirectStream": its end is told to the server by
/// the bool of the load, not by that name. Before, the player took a
/// stream for a transcode by the name alone, and the ffmpeg of a
/// remux ran on after the stop.
#[test]
fn the_stop_of_a_remux_ends_its_stream_on_the_server() {
    use std::io::{Read, Write};
    let player = Player::default();
    let generation = player.prepare("Remux", 0., false);
    // What the worker sends at the stop: one report, with the bool.
    let kinds = |transcoding: bool, method: &str| {
        let (mut worker, reports) = worker_of(generation);
        let mut session = Some(Session { transcoding, play_method: method.into(), ..session_of("remux") });
        finish(&player.shared, &mut session, &mut worker);
        reports.try_iter().collect::<Vec<Report>>()
    };
    let ends = |sent: &[Report]| sent.iter().map(|r| (r.kind == ReportKind::Stopped, r.transcoding)).collect::<Vec<_>>();
    let sent = kinds(true, "DirectStream");
    assert_eq!(ends(&sent), [(true, true)], "the stop of a remux does not end its stream");
    assert_eq!(sent[0].progress.play_session_id, "session");
    // A transcode is ended the same way, a direct play has nothing to end.
    assert_eq!(ends(&kinds(true, "Transcode")), [(true, true)]);
    assert_eq!(ends(&kinds(false, "DirectPlay")), [(true, false)]);

    // What the reporter does with the stop of the remux: the report,
    // then one DELETE of the encoding. A stop with no stream of the
    // server is the report alone.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (seen_tx, seen_rx) = mpsc::channel::<String>();
    thread::spawn(move || {
        for mut socket in listener.incoming().flatten() {
            let _ = socket.set_read_timeout(Some(Duration::from_secs(3)));
            let mut buffer = [0u8; 2048];
            let n = socket.read(&mut buffer).unwrap_or(0);
            let head = String::from_utf8_lossy(&buffer[..n]);
            let _ = seen_tx.send(head.lines().next().unwrap_or_default().to_string());
            let _ = socket.write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        }
    });
    let client = Client::new(&format!("http://127.0.0.1:{port}"), "device-1").with_session("token", "user");
    let (reports, thread) = reporter();
    for mut stop in sent.into_iter().chain(kinds(false, "DirectPlay")) {
        stop.client = client.clone();
        reports.send(stop).unwrap();
    }
    drop(reports);
    thread.unwrap().join().unwrap();
    let lines: Vec<String> = seen_rx.try_iter().filter(|line| !line.contains("/Sessions/Playing/Stopped")).collect();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].starts_with("DELETE /Videos/ActiveEncodings") && lines[0].contains("playSessionId=session"), "{lines:?}");
}

/// The group and its factor on the speed can be read back
/// (`sync_state`), and a new load starts at the factor 1.
#[test]
fn the_sync_state_is_what_the_group_set() {
    let player = Player::default();
    assert_eq!(player.sync_state(), (false, 1.));
    player.set_group(true);
    player.set_sync_speed(1.05);
    assert_eq!(player.sync_state(), (true, 1.05));
    player.set_group(false);
    player.set_sync_speed(1.);
    assert_eq!(player.sync_state(), (false, 1.));
}

/// An exact seek that comes while a load still waits for its answer
/// does not take the answer away: the load fails with the item, and
/// is not left with no answer.
#[test]
fn a_seek_over_a_pending_load_keeps_the_answer_of_the_load() {
    let player = Player::default();
    let generation = player.prepare("Item", 0., false);
    let (mut worker, _reports) = worker_of(generation);
    let mut session = Some(session_of("item"));
    worker.pending = Some(Pending { token: 7, target: None, restarted: None, load: None });
    worker.await_seek(12., 8);
    let pending = worker.pending.as_ref().unwrap();
    assert_eq!((pending.token, pending.target, pending.load_token()), (8, Some(12.), Some(7)));
    // A second seek keeps it as well.
    worker.await_seek(15., 9);
    assert_eq!(worker.pending.as_ref().unwrap().load_token(), Some(7));
    finish(&player.shared, &mut session, &mut worker);
    assert_eq!(events_of(&player), vec![PlayerEvent::LoadFailed { token: 7 }]);
    // A seek of an item that plays has no load to answer.
    worker.await_seek(3., 10);
    assert_eq!(worker.pending.as_ref().unwrap().load_token(), None);
}

/// Only the error of a file that ended ends the item. The wrapper
/// gives an error also for a log line of mpv that is not UTF-8, and
/// that ended the playback with "Playback failed".
#[test]
fn an_event_that_cannot_be_read_does_not_end_the_item() {
    assert_eq!(end_error(&libmpv2::Error::InvalidUtf8), None);
    assert_eq!(end_error(&libmpv2::Error::Null), None);
    assert!(end_error(&libmpv2::Error::Raw(sys::mpv_error_MPV_ERROR_LOADING_FAILED)).is_some());
}

/// A subtitle file is fetched once: a choice of it while it is on its
/// way marks it to be shown and starts no second fetch (which wrote
/// the same file again, under mpv). The "Loading…" mark is of one
/// load: the file of the load before does not clear the mark of the
/// same index in the load that replaced it.
#[test]
fn a_subtitle_file_on_its_way_is_not_fetched_again() {
    let file = |index: i64, select: bool, graphical: bool| SubtitleFile {
        index,
        url: String::new(),
        title: String::new(),
        lang: String::new(),
        select,
        graphical,
    };
    let indexes = |files: Vec<SubtitleFile>| files.iter().map(|f| f.index).collect::<Vec<_>>();
    let mut fetching = Vec::new();
    // At the start: the text file, and no image track that is not shown.
    assert_eq!(indexes(wanted_files(&mut fetching, 1, vec![file(2, false, false), file(3, false, true)])), vec![2]);
    // The text file is chosen while on its way, twice; the image track once.
    assert_eq!(indexes(wanted_files(&mut fetching, 1, vec![file(2, true, false)])), Vec::<i64>::new());
    assert_eq!(indexes(wanted_files(&mut fetching, 1, vec![file(2, true, false)])), Vec::<i64>::new());
    assert_eq!(indexes(wanted_files(&mut fetching, 1, vec![file(3, true, true)])), vec![3]);
    assert_eq!(fetch_over(&mut fetching, 1, 2), Some(true), "the choice made on the way was lost");
    // Once it is over it can be fetched again (it failed, and is chosen again).
    assert_eq!(indexes(wanted_files(&mut fetching, 1, vec![file(2, true, false)])), vec![2]);
    // The next load starts with a clean list, and the fetch of the old one finds nothing.
    assert_eq!(indexes(wanted_files(&mut fetching, 2, vec![file(3, false, false)])), vec![3]);
    assert_eq!(fetch_over(&mut fetching, 1, 3), None);
    assert_eq!(fetch_over(&mut fetching, 2, 3), Some(false));

    let player = Player::default();
    let old = player.prepare("Item", 0., false);
    let new = player.prepare("Item again", 0., false);
    set_pending(&player.shared, new, 2, true);
    set_pending(&player.shared, old, 2, false);
    assert_eq!(player.status().pending_subs, vec![2], "the file of the load before cleared the mark");
    set_pending(&player.shared, new, 2, false);
    assert!(player.status().pending_subs.is_empty());
}

/// The quit of the app with an item in the player: the server hears
/// of the stop before the worker is gone. Before, the worker ended
/// with no report, and the server kept the session as playing.
#[test]
fn a_quit_reports_the_stop_of_the_item() {
    use std::io::{Read, Write};
    let Some(_one) = real_player_turn() else { return };
    if crate::jellyfin::no_report() {
        eprintln!("\nSKIPPED (BLOOM_NO_REPORT is set): a_quit_reports_the_stop_of_the_item");
        return;
    }
    let Some(media) = silent_clip("bloom-quit-report-test.mp4") else { return };
    // A server that takes every report and keeps its first line.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    {
        let seen = seen.clone();
        thread::spawn(move || {
            for mut socket in listener.incoming().flatten() {
                let _ = socket.set_read_timeout(Some(Duration::from_secs(3)));
                let mut buffer = [0u8; 4096];
                let n = socket.read(&mut buffer).unwrap_or(0);
                let head = String::from_utf8_lossy(&buffer[..n]);
                seen.lock().unwrap().push(head.lines().next().unwrap_or_default().to_string());
                let _ = socket.write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            }
        });
    }
    let player = Player::default();
    let closer = Closer(player.clone());
    player.set_target_size(320, 240);
    player.set_muted(true);
    let mut request = request(&media.display().to_string(), "Quit", 101);
    request.client = Client::new(&format!("http://127.0.0.1:{port}"), "device-1").with_session("token", "user");
    player.play(request);
    expect(&player, "loaded", |e| *e == PlayerEvent::Loaded { token: 101 });
    // The quit: the worker is gone when this returns.
    drop(closer);
    assert!(player.commands.lock().unwrap().is_none(), "the worker did not end");
    let seen = seen.lock().unwrap().clone();
    assert!(seen.iter().any(|line| line.starts_with("POST /Sessions/Playing/Stopped")), "no stop report at the quit: {seen:?}");
    assert_eq!(player.status().state, PlayState::Ended);
}

/// The same file chosen twice while it comes, at a server that takes
/// its time: one request, one track, and the track is shown.
#[test]
fn a_file_chosen_twice_while_it_comes_is_one_track() {
    let Some(_one) = real_player_turn() else { return };
    let Some(clip) = silent_clip("bloom-test-chosen-twice.mp4") else { return };
    const DELAY: Duration = Duration::from_secs(2);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let requests = Arc::new(AtomicU32::new(0));
    {
        let requests = requests.clone();
        thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                requests.fetch_add(1, Ordering::Relaxed);
                thread::spawn(move || {
                    let mut request = [0u8; 4096];
                    let _ = std::io::Read::read(&mut stream, &mut request);
                    thread::sleep(DELAY);
                    let body = "1\n00:00:00,000 --> 00:00:30,000\ntwice\n";
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = std::io::Write::write_all(&mut stream, format!("{head}{body}").as_bytes());
                });
            }
        });
    }
    let file = |select: bool| SubtitleFile {
        index: 2,
        url: format!("http://127.0.0.1:{port}/Subtitles/2/0/Stream.srt"),
        title: "twice".into(),
        lang: "eng".into(),
        select,
        graphical: false,
    };
    let player = Player::default();
    let _closer = Closer(player.clone());
    let mut request = request(&clip.to_string_lossy(), "chosen twice", 1);
    request.subtitles = vec![file(false)];
    player.play(request);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !player.status().pending_subs.contains(&2) {
        assert!(Instant::now() < deadline, "the file was never on its way: {:?}", player.status());
        thread::sleep(Duration::from_millis(20));
    }
    player.fetch_subtitle_file(file(true));
    player.fetch_subtitle_file(file(true));
    let deadline = Instant::now() + DELAY + Duration::from_secs(10);
    let of_file = |status: &PlayerStatus| {
        status.tracks.iter().filter(|t| t.kind == TrackKind::Sub && t.title.as_deref() == Some("twice")).cloned().collect::<Vec<_>>()
    };
    while of_file(&player.status()).is_empty() {
        assert!(Instant::now() < deadline, "the subtitle never came: {:?}", player.status());
        thread::sleep(Duration::from_millis(50));
    }
    // A second fetch would come within its own delay.
    thread::sleep(DELAY + Duration::from_millis(500));
    let status = player.status();
    let tracks = of_file(&status);
    assert_eq!(requests.load(Ordering::Relaxed), 1, "the file was asked for more than once");
    assert_eq!(tracks.len(), 1, "{tracks:?}");
    assert!(tracks[0].selected, "the choice made while the file came was lost: {tracks:?}");
    assert!(status.pending_subs.is_empty(), "{status:?}");
    player.stop();
}
