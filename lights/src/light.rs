// SPDX-License-Identifier: AGPL-3.0-or-later
//! One light as the rules drive it, of any brand: it keeps what the lamp
//! had and puts it back, fades by the lamp's own glide, and glides the
//! colours of the picture into the lamp's frame stream. A driver only
//! sends single commands ([`crate::driver::Lamp`]).

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use crate::{
    driver::{Lamp, LampState},
    rules::Action,
};

/// The time a light takes to be back at what it had, after the commands
/// of a restore: its own glide, with a margin.
const RESTORE_TIME: Duration = Duration::from_millis(1500);
/// Frames a second, and the glide into a film and out of it, in seconds.
const STREAM_HZ: u64 = 30;
const STREAM_SOFT: f64 = 0.35;

/// The frame stream of the light, while the light follows the picture.
#[derive(Default)]
struct Stream {
    on: bool,
    /// Counts the starts of the stream: the thread of an older start ends.
    starts: u64,
    /// When the stream began: its first second glides slowly.
    since: Option<Instant>,
    glide: f64,
    /// Where the light goes and where it is, the bottom of the light first.
    target: Vec<[f64; 3]>,
    shown: Vec<[f64; 3]>,
}

/// One light.
#[derive(Clone)]
pub struct Light {
    lamp: Arc<dyn Lamp>,
    /// The brightness last sent, and what the lamp had before the film.
    level: Arc<Mutex<f64>>,
    before: Arc<Mutex<Option<LampState>>>,
    /// What the last restore put back, and when: a start that comes while
    /// the lamp still glides there (the next item of a queue, a film
    /// played again at once) keeps this, and does not ask the lamp, which
    /// would answer with a level in the middle of the glide.
    restored: Arc<Mutex<Option<(Instant, Option<LampState>)>>>,
    /// Counts the fades: a fade ends when a newer one started.
    fades: Arc<AtomicU64>,
    /// The fades that still have a command to send.
    waiting: Arc<AtomicU64>,
    /// The lamp was told something since `Remember`: only then is there
    /// something to put back.
    touched: Arc<std::sync::atomic::AtomicBool>,
    /// The lamp was off, and was switched on to follow the picture: it is
    /// switched off again at the end.
    woken: Arc<std::sync::atomic::AtomicBool>,
    stream: Arc<Mutex<Stream>>,
}

/// The colour a light shows for a colour of the picture. A light shows a
/// pale colour as white, so the colour is made stronger; a dark picture is
/// a dim light, with a floor so the light never goes out.
fn light_colour([r, g, b]: [u8; 3]) -> [f64; 3] {
    let (max, min) = (f64::from(r.max(g).max(b)), f64::from(r.min(g).min(b)));
    if max == 0. {
        return [10.; 3];
    }
    let strength = ((max - min) / max * 2.2).min(1.);
    let value = (max / 255.).powf(1.3).max(0.04) * 255.;
    // The same hue: each part keeps its place between the least and the most.
    [r, g, b].map(|part| {
        let place = if max == min { 1. } else { (f64::from(part) - min) / (max - min) };
        value * (1. - strength * (1. - place))
    })
}

/// A white of a colour temperature, as red, green and blue (a rough fit,
/// enough for a light between a candle and the sky).
fn white(kelvin: u32) -> [f64; 3] {
    let warm = 1. - (f64::from(kelvin.clamp(2000, 6500)) - 2000.) / 4500.;
    [255., 255. - 95. * warm, 255. - 215. * warm]
}

/// The levels a fade tells the light: the target alone on the way up; on
/// the way down the level half-way for the eye first (the eye sees
/// brightness about as its square root), then the target.
fn fade_steps(from: f64, to: f64) -> Vec<u32> {
    let whole = |level: f64| level.round().clamp(1., 100.) as u32;
    let (first, last) = (whole(from), whole(to));
    let middle = whole(((f64::from(first).sqrt() + f64::from(last).sqrt()) / 2.).powi(2));
    if last < middle && middle < first { vec![middle, last] } else { vec![last] }
}

impl Light {
    pub fn new(lamp: Arc<dyn Lamp>) -> Self {
        Self {
            lamp,
            level: Arc::new(Mutex::new(100.)),
            before: Arc::new(Mutex::new(None)),
            restored: Arc::new(Mutex::new(None)),
            fades: Arc::new(AtomicU64::new(0)),
            waiting: Arc::new(AtomicU64::new(0)),
            touched: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            woken: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            stream: Arc::new(Mutex::new(Stream::default())),
        }
    }

    /// The light the lamp had at `Remember`, as a colour of the stream.
    fn own_light(&self) -> [f64; 3] {
        let before = self.before.lock().unwrap();
        let Some(before) = &*before else { return [60.; 3] };
        // A lamp that was off comes out of the dark, and goes back to it.
        if !before.on {
            return [0.; 3];
        }
        let colour = match (before.kelvin, before.colour) {
            (Some(kelvin), _) => white(kelvin),
            (None, Some(colour)) => colour.map(f64::from),
            (None, None) => [255.; 3],
        };
        colour.map(|part| part * before.level / 100.)
    }

    /// Shows colours along the lamp, the bottom first, in the frame
    /// stream; the first call takes the lamp into the stream, from the
    /// light it had. Nothing for a lamp with no frame stream.
    fn stream_to(&self, target: Vec<[f64; 3]>, glide: f64) {
        let colours = self.lamp.frame_colours();
        if colours == 0 {
            return;
        }
        let mut stream = self.stream.lock().unwrap();
        stream.target = target;
        stream.glide = glide;
        if stream.on {
            return;
        }
        *stream = Stream {
            on: true,
            starts: stream.starts + 1,
            since: Some(Instant::now()),
            shown: vec![self.own_light(); colours],
            ..std::mem::take(&mut *stream)
        };
        let mine = stream.starts;
        self.touched.store(true, Ordering::Release);
        // A lamp that is off shows no frame: it is switched on for the
        // picture, after the stream began, so its first light is the dark
        // first frame and not the light it kept from before.
        let wake = self.off_before() && !self.woken.swap(true, Ordering::AcqRel);
        // Sent with the lock held, as the frames are: the lamp gets the
        // start, the frames and the end in that order.
        self.lamp.stream(true);
        if wake {
            self.lamp.frame(&vec![[0; 3]; colours]);
            self.lamp.power(true);
        }
        drop(stream);
        let light = self.clone();
        thread::spawn(move || {
            let mut last = Instant::now();
            loop {
                thread::sleep(Duration::from_millis(1000 / STREAM_HZ));
                let mut stream = light.stream.lock().unwrap();
                if !stream.on || stream.starts != mine {
                    return;
                }
                let soft = stream.since.is_none_or(|since| since.elapsed() < Duration::from_secs(1));
                let glide = if soft { stream.glide.max(STREAM_SOFT) } else { stream.glide };
                let part = 1. - (-last.elapsed().as_secs_f64() / glide).exp();
                last = Instant::now();
                let Stream { shown, target, .. } = &mut *stream;
                let frame: Vec<[u8; 3]> = shown
                    .iter_mut()
                    .enumerate()
                    .map(|(at, shown)| {
                        let to = target.get(at * target.len() / colours).copied().unwrap_or(*shown);
                        for (now, to) in shown.iter_mut().zip(to) {
                            *now += (to - *now) * part;
                        }
                        shown.map(|part| part.round().clamp(0., 255.) as u8)
                    })
                    .collect();
                light.lamp.frame(&frame);
            }
        });
    }

    /// Leaves the frame stream, by a glide to the light the lamp had: the
    /// lamp goes back to that light when the stream ends.
    fn leave_stream(&self) {
        if !self.streams() {
            return;
        }
        self.stream_to(vec![self.own_light()], STREAM_SOFT);
        thread::sleep(Duration::from_secs_f64(STREAM_SOFT * 3.));
        let mut stream = self.stream.lock().unwrap();
        stream.on = false;
        // A lamp that was switched on for the picture goes off while it
        // still shows the dark end of the glide: the end of the stream
        // would show the light it kept from before for a moment.
        if self.woken.swap(false, Ordering::AcqRel) {
            self.lamp.power(false);
        }
        self.lamp.stream(false);
    }

    /// Whether the lamp was off when it was asked.
    fn off_before(&self) -> bool {
        self.before.lock().unwrap().as_ref().is_some_and(|before| !before.on)
    }

    /// A lamp that was off stays off for a film that only dims the lights:
    /// off is dim enough, and a brightness command would switch it on.
    fn sleeps(&self) -> bool {
        self.off_before() && !self.woken.load(Ordering::Acquire)
    }

    /// Whether the light is in the frame stream.
    pub fn streams(&self) -> bool {
        self.stream.lock().unwrap().on
    }

    /// Goes to a brightness. The lamp glides to each level it is told
    /// (a Govee lamp in well under a second, and it cannot be set), so the
    /// fade is the lamp's own: one command on the way up, and on the way
    /// down two, with the time of a glide between them. Ends early when a
    /// newer fade started.
    ///
    /// Tried on a Govee H6076 on 2026-10-08. Many small steps looked
    /// worse, slow or fast: each command starts the glide again, which
    /// showed as a stutter on the wall. One command down was smooth but
    /// too quick; two commands 0.4 seconds apart were smooth and slower.
    /// On the way up the second command showed as a stutter at the
    /// start, and one command looked right.
    fn fade(&self, to: f64, gap: Duration) {
        // The first command goes out here, before this returns: what the
        // caller sends next (a colour, the start of the frame stream)
        // comes after it. Counted and sent under the lock a fade sends
        // under, so no older fade sends after this one began.
        let (mine, rest) = {
            let mut level = self.level.lock().unwrap();
            let mine = self.fades.fetch_add(1, Ordering::AcqRel) + 1;
            let mut steps = fade_steps(*level, to.clamp(1., 100.)).into_iter();
            self.touched.store(true, Ordering::Release);
            if let Some(first) = steps.next() {
                self.lamp.level(first);
                *level = f64::from(first);
            }
            (mine, steps.collect::<Vec<u32>>())
        };
        if rest.is_empty() {
            return;
        }
        let light = self.clone();
        self.waiting.fetch_add(1, Ordering::AcqRel);
        thread::spawn(move || {
            for value in rest {
                thread::sleep(gap);
                let mut level = light.level.lock().unwrap();
                if light.fades.load(Ordering::Acquire) != mine {
                    break;
                }
                light.lamp.level(value);
                *level = f64::from(value);
            }
            light.waiting.fetch_sub(1, Ordering::AcqRel);
        });
    }

    /// Whether a fade still has a command to send. The program must not
    /// end before it has: a light on its way back would stop half-way.
    pub fn busy(&self) -> bool {
        self.waiting.load(Ordering::Acquire) > 0
    }

    /// Ends a fade that still has a command to send: the frame stream
    /// begins, and a brightness command must not come in the middle of it.
    fn end_fades(&self) {
        let _level = self.level.lock().unwrap();
        self.fades.fetch_add(1, Ordering::AcqRel);
    }

    /// Shows the colour, or the white, the lamp had at `Remember`. A lamp
    /// that did not say its colour gets none back.
    fn own_colour(&self, before: &LampState) {
        match (before.kelvin, before.colour) {
            (Some(kelvin), _) => self.lamp.white(kelvin),
            (None, Some(colour)) => self.lamp.colour(colour),
            (None, None) => {}
        }
    }

    pub fn act(&self, action: Action, gap: Duration, glide: f64) {
        match action {
            Action::Aura(colours) => {
                self.end_fades();
                // The first colour of a frame is the bottom of the lamp.
                self.stream_to(colours.into_iter().rev().map(light_colour).collect(), glide);
            }
            Action::AuraWhite { kelvin, level } => {
                self.end_fades();
                self.stream_to(vec![white(kelvin).map(|part| part * level.clamp(1., 100.) / 100.)], STREAM_SOFT);
            }
            Action::Remember => {
                let restored = self.restored.lock().unwrap().take();
                if let Some((_, before)) = restored.filter(|(at, _)| at.elapsed() < RESTORE_TIME + gap) {
                    *self.before.lock().unwrap() = before;
                    return;
                }
                self.touched.store(false, Ordering::Release);
                let state = self.lamp.state();
                if let Some(state) = &state {
                    *self.level.lock().unwrap() = state.level;
                }
                *self.before.lock().unwrap() = state;
            }
            Action::Colour(_) | Action::OwnColour | Action::Fade { .. } if self.sleeps() => {}
            Action::Colour(colour) => {
                self.touched.store(true, Ordering::Release);
                self.lamp.colour(colour);
            }
            Action::OwnColour => {
                if let Some(before) = &*self.before.lock().unwrap() {
                    self.own_colour(before);
                }
            }
            Action::Fade { to, kelvin } => {
                if let Some(kelvin) = kelvin {
                    self.touched.store(true, Ordering::Release);
                    self.lamp.white(kelvin);
                }
                self.fade(to, gap);
            }
            Action::Restore => {
                // A lamp that was off: the picture switched it on, and the
                // way out of the stream switches it off. Nothing else of
                // it was changed.
                let was_off = self.off_before();
                self.leave_stream();
                let before = self.before.lock().unwrap().take();
                if was_off {
                    self.touched.store(false, Ordering::Release);
                    *self.restored.lock().unwrap() = Some((Instant::now(), before));
                    return;
                }
                // A lamp that was told nothing has nothing to get back (a
                // lamp with no frame stream while the picture is followed):
                // it is not set to full light for an answer that never came.
                if !self.touched.swap(false, Ordering::AcqRel) {
                    return;
                }
                *self.restored.lock().unwrap() = Some((Instant::now(), before.clone()));
                if let Some(before) = &before {
                    self.own_colour(before);
                }
                // With no answer of the lamp at the start, full light.
                self.fade(before.map_or(100., |before| before.level), gap);
            }
        }
    }

    /// Makes the lamp show which one it is: bright, dim, bright, dim, and
    /// then what it had. A lamp that was off is switched on for it, and
    /// off again.
    pub fn identify(&self) {
        let before = self.lamp.state();
        let was_off = before.as_ref().is_some_and(|before| !before.on);
        if was_off {
            self.lamp.power(true);
        }
        for level in [100, 5, 100, 5] {
            self.lamp.level(level);
            thread::sleep(Duration::from_millis(600));
        }
        self.lamp.level(before.map_or(100, |before| before.level.round() as u32));
        if was_off {
            // After the level: the lamp keeps it for the next time it is on.
            thread::sleep(Duration::from_millis(300));
            self.lamp.power(false);
        }
    }
}

/// The lights the user chose: each does what the rules ask.
#[derive(Clone, Default)]
pub struct Lights(pub Vec<Light>);

impl Lights {
    pub fn act(&self, action: &Action, gap: Duration, glide: f64) {
        // Each light in its own thread, so the lights change together. The
        // caller goes on when all are done, so the order of the actions
        // holds for every light: a lamp that does not answer the question
        // for its state holds the next action of all up for its second.
        thread::scope(|scope| {
            for light in &self.0 {
                scope.spawn(|| light.act(action.clone(), gap, glide));
            }
        });
    }

    pub fn streams(&self) -> bool {
        self.0.iter().any(Light::streams)
    }

    /// Waits until no light has a command of a fade left to send, for at
    /// most `most`.
    pub fn settle(&self, most: Duration) {
        let until = Instant::now() + most;
        while self.0.iter().any(Light::busy) && Instant::now() < until {
            thread::sleep(Duration::from_millis(20));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a lamp of a test was told.
    #[derive(Clone, Debug, PartialEq)]
    enum Told {
        Asked,
        Power(bool),
        Level(u32),
        Colour([u8; 3]),
        White(u32),
        Stream(bool),
        Frame(Vec<[u8; 3]>),
    }

    /// A lamp that keeps what it is told, and says it has 80% of a warm
    /// colour (or nothing, for a lamp that does not answer).
    struct Mock {
        told: Mutex<Vec<Told>>,
        answers: bool,
        frame: usize,
        /// Whether the lamp says it is switched on.
        on: bool,
    }

    impl Mock {
        fn light(answers: bool, frame: usize) -> (Arc<Mock>, Light) {
            let mock = Arc::new(Mock { told: Mutex::new(Vec::new()), answers, frame, on: true });
            (mock.clone(), Light::new(mock))
        }
        /// A lamp that is switched off, and says so.
        fn off() -> (Arc<Mock>, Light) {
            let mock = Arc::new(Mock { told: Mutex::new(Vec::new()), answers: true, frame: 4, on: false });
            (mock.clone(), Light::new(mock))
        }
        fn told(&self) -> Vec<Told> {
            self.told.lock().unwrap().clone()
        }
        fn levels(&self) -> Vec<u32> {
            self.told().iter().filter_map(|told| if let Told::Level(level) = told { Some(*level) } else { None }).collect()
        }
        fn note(&self, told: Told) {
            self.told.lock().unwrap().push(told);
        }
    }

    impl Lamp for Mock {
        fn state(&self) -> Option<LampState> {
            self.note(Told::Asked);
            self.answers.then_some(LampState { on: self.on, level: 80., colour: Some([255, 200, 100]), kelvin: None })
        }
        fn power(&self, on: bool) {
            self.note(Told::Power(on));
        }
        fn level(&self, percent: u32) {
            self.note(Told::Level(percent));
        }
        fn colour(&self, colour: [u8; 3]) {
            self.note(Told::Colour(colour));
        }
        fn white(&self, kelvin: u32) {
            self.note(Told::White(kelvin));
        }
        fn frame_colours(&self) -> usize {
            self.frame
        }
        fn stream(&self, on: bool) {
            self.note(Told::Stream(on));
        }
        fn frame(&self, colours: &[[u8; 3]]) {
            self.note(Told::Frame(colours.to_vec()));
        }
    }

    const NO_GAP: Duration = Duration::ZERO;

    /// Down in two commands with the level half-way for the eye between
    /// them, up in one.
    #[test]
    fn a_fade_goes_down_in_two_commands_and_up_in_one() {
        assert_eq!(fade_steps(72., 5.), [29, 5]);
        assert_eq!(fade_steps(25., 5.), [13, 5]);
        assert_eq!(fade_steps(5., 72.), [72]);
        assert_eq!(fade_steps(40., 40.), [40]);
        // No level lies between the two: one command.
        assert_eq!(fade_steps(6., 5.), [5]);
    }

    /// The lamp is asked for its state, faded down from where it was, and
    /// put back to the brightness and the colour it had. A newer fade ends
    /// the one that runs, and the first command of a fade is out before
    /// the next thing is sent.
    #[test]
    fn the_light_is_remembered_faded_and_put_back() {
        let (lamp, light) = Mock::light(true, 14);
        light.act(Action::Remember, NO_GAP, 0.05);
        light.act(Action::Fade { to: 5., kelvin: None }, Duration::from_millis(120), 0.05);
        // The first command is there at once, the second after the gap.
        assert_eq!(lamp.told(), [Told::Asked, Told::Level(31)]);
        thread::sleep(Duration::from_millis(400));
        assert_eq!(lamp.levels(), [31, 5], "not the half-way level and the target");
        // The colour of a film, the lamp's own colour, the white of a pause.
        light.act(Action::Colour([0, 80, 255]), NO_GAP, 0.05);
        light.act(Action::OwnColour, NO_GAP, 0.05);
        light.act(Action::Fade { to: 25., kelvin: Some(2700) }, NO_GAP, 0.05);
        assert_eq!(lamp.told()[3..], [Told::Colour([0, 80, 255]), Told::Colour([255, 200, 100]), Told::White(2700), Told::Level(25)]);
        // A slow fade down, and the end of the film in its gap: the second
        // command of that fade never comes.
        light.act(Action::Fade { to: 5., kelvin: None }, Duration::from_millis(400), 0.05);
        thread::sleep(Duration::from_millis(100));
        light.act(Action::Restore, Duration::from_millis(120), 0.05);
        thread::sleep(Duration::from_millis(900));
        assert_eq!(lamp.levels(), [31, 5, 25, 13, 80], "the old fade ran on, or the light is not back");
        assert_eq!(lamp.told().iter().rev().find(|told| matches!(told, Told::Colour(_))), Some(&Told::Colour([255, 200, 100])));
        // The next film starts while the lamp still glides back: the lamp
        // is not asked again (it would answer with a level on the way).
        let asked = |lamp: &Mock| lamp.told().iter().filter(|told| **told == Told::Asked).count();
        light.act(Action::Remember, NO_GAP, 0.05);
        assert_eq!(asked(&lamp), 1, "the lamp was asked in the middle of its restore");
        assert_eq!(light.before.lock().unwrap().as_ref().map(|before| before.level), Some(80.));
        // Later it is asked again: a person can have changed the lamp.
        light.act(Action::Restore, NO_GAP, 0.05);
        thread::sleep(RESTORE_TIME + Duration::from_millis(100));
        light.act(Action::Remember, NO_GAP, 0.05);
        assert_eq!(asked(&lamp), 2);
    }

    /// A lamp that does not answer: full light at the end, no colour (none
    /// is known), and a start in the middle of that restore keeps "nothing
    /// known" in place of an answer from the middle of the glide.
    #[test]
    fn a_lamp_that_does_not_answer_gets_full_light_back() {
        let (lamp, light) = Mock::light(false, 14);
        light.act(Action::Remember, NO_GAP, 0.05);
        light.act(Action::Fade { to: 5., kelvin: None }, NO_GAP, 0.05);
        thread::sleep(Duration::from_millis(50));
        light.act(Action::Restore, NO_GAP, 0.05);
        assert_eq!(lamp.told().last(), Some(&Told::Level(100)));
        assert!(!lamp.told().iter().any(|told| matches!(told, Told::Colour(_) | Told::White(_))));
        light.act(Action::Remember, NO_GAP, 0.05);
        assert_eq!(lamp.told().iter().filter(|told| **told == Told::Asked).count(), 1);
    }

    /// The light goes into the frame stream from the light it had, shows
    /// the picture with its top at the top of the lamp (the last colour of
    /// a frame), ends a fade that still has a command to send, and leaves
    /// the stream by a glide to the light it had.
    #[test]
    fn the_stream_starts_shows_the_picture_and_ends() {
        let (lamp, light) = Mock::light(true, 4);
        light.act(Action::Remember, NO_GAP, 0.05);
        light.act(Action::Fade { to: 5., kelvin: None }, Duration::from_millis(300), 0.05);
        // The top of the picture is red, its bottom blue.
        light.act(Action::Aura(vec![[255, 0, 0], [0, 0, 255]]), NO_GAP, 0.01);
        thread::sleep(Duration::from_millis(1500));
        assert_eq!(lamp.levels(), [31], "a fade went on into the stream");
        let frames = |lamp: &Mock| -> Vec<Vec<[u8; 3]>> {
            lamp.told().into_iter().filter_map(|told| if let Told::Frame(frame) = told { Some(frame) } else { None }).collect()
        };
        let shown = frames(&lamp);
        assert!(shown.len() > 30, "only {} frames", shown.len());
        assert!(shown.iter().all(|frame| frame.len() == 4));
        // The first frame is near the light the lamp had (80% of its
        // colour), the last one is the picture: blue below, red above.
        let (first, last) = (&shown[0], shown.last().unwrap());
        assert!(first[0][0] > 150 && first[0][1] > 100, "{first:?}");
        assert!(last[0][2] > 200 && last[0][0] < 30 && last[3][0] > 200 && last[3][2] < 30, "{last:?}");
        // The white of a pause is one light along the whole lamp.
        light.act(Action::AuraWhite { kelvin: 2700, level: 25. }, NO_GAP, 0.05);
        thread::sleep(Duration::from_millis(1500));
        let pause = frames(&lamp).last().unwrap().clone();
        // (The glide to it is still on its last steps.)
        let near = |a: [u8; 3], b: [u8; 3]| a.iter().zip(b).all(|(a, b)| a.abs_diff(b) <= 8);
        assert!(pause.iter().all(|colour| near(*colour, pause[0])), "{pause:?}");
        assert!((55..=72).contains(&pause[0][0]) && pause[0][2] < pause[0][0], "{pause:?}");
        light.act(Action::Restore, NO_GAP, 0.05);
        let told = lamp.told();
        let starts: Vec<&Told> = told.iter().filter(|told| matches!(told, Told::Stream(_))).collect();
        assert_eq!(starts, [&Told::Stream(true), &Told::Stream(false)]);
        // After the end of the stream: the colour and the level it had,
        // and no frame.
        let end = told.iter().position(|told| *told == Told::Stream(false)).unwrap();
        assert_eq!(told[end + 1..], [Told::Colour([255, 200, 100]), Told::Level(80)]);
        // The last frame before the end is the light it had again.
        let back = frames(&lamp).last().unwrap().clone();
        assert!(back[0][0] > 150 && back[0][1] > 100, "{back:?}");
        thread::sleep(Duration::from_millis(100));
        assert_eq!(lamp.told().len(), told.len(), "frames after the end of the stream");
    }

    /// A stream that is left and entered again at once has one sender, not
    /// two: the frames come at the rate of one.
    #[test]
    fn a_stream_entered_again_has_one_sender() {
        let (lamp, light) = Mock::light(true, 2);
        light.act(Action::Remember, NO_GAP, 0.05);
        for _ in 0..3 {
            light.act(Action::Aura(vec![[9, 9, 9]]), NO_GAP, 0.05);
            // The end of the stream without its glide, as fast as can be.
            light.stream.lock().unwrap().on = false;
        }
        light.act(Action::Aura(vec![[9, 9, 9]]), NO_GAP, 0.05);
        let count = |lamp: &Mock| lamp.told().iter().filter(|told| matches!(told, Told::Frame(_))).count();
        thread::sleep(Duration::from_millis(200));
        let before = count(&lamp);
        thread::sleep(Duration::from_secs(1));
        let frames = count(&lamp) - before;
        assert!((20..=34).contains(&frames), "{frames} frames in a second: more than one sender, or none");
        light.act(Action::Restore, NO_GAP, 0.05);
    }

    /// A lamp with no frame stream is left alone by the picture and by the
    /// white of a pause: it was told nothing, and gets nothing "back" at
    /// the end, also when it did not say what it had.
    #[test]
    fn a_lamp_with_no_stream_does_not_follow_the_picture() {
        for answers in [true, false] {
            let (lamp, light) = Mock::light(answers, 0);
            light.act(Action::Remember, NO_GAP, 0.05);
            light.act(Action::Aura(vec![[255, 0, 0]]), NO_GAP, 0.05);
            light.act(Action::AuraWhite { kelvin: 2700, level: 25. }, NO_GAP, 0.05);
            thread::sleep(Duration::from_millis(100));
            light.act(Action::Restore, NO_GAP, 0.05);
            assert_eq!(lamp.told(), [Told::Asked]);
        }
    }

    /// A lamp that is off stays off for a film that dims the lights: it is
    /// told nothing, at the start, at a pause and at the end.
    #[test]
    fn a_lamp_that_is_off_stays_off_for_a_film_that_dims() {
        let (lamp, light) = Mock::off();
        light.act(Action::Remember, NO_GAP, 0.05);
        light.act(Action::Colour([1, 2, 3]), NO_GAP, 0.05);
        light.act(Action::Fade { to: 5., kelvin: None }, NO_GAP, 0.05);
        light.act(Action::Fade { to: 25., kelvin: Some(2700) }, NO_GAP, 0.05);
        light.act(Action::OwnColour, NO_GAP, 0.05);
        light.act(Action::Restore, NO_GAP, 0.05);
        assert_eq!(lamp.told(), [Told::Asked]);
    }

    /// A lamp that is off is switched on to follow the picture, out of the
    /// dark and after the stream began, and is switched off again at the
    /// end, before the stream ends. Its brightness and colour are not
    /// touched.
    #[test]
    fn a_lamp_that_is_off_follows_the_picture_and_goes_off_again() {
        let (lamp, light) = Mock::off();
        light.act(Action::Remember, NO_GAP, 0.05);
        light.act(Action::Aura(vec![[255, 0, 0]]), NO_GAP, 0.01);
        thread::sleep(Duration::from_millis(300));
        let told = lamp.told();
        assert_eq!(told[..4], [Told::Asked, Told::Stream(true), Told::Frame(vec![[0; 3]; 4]), Told::Power(true)]);
        // The next frames come out of the dark, not from the light it kept.
        let Told::Frame(first) = &told[4] else { panic!("{:?}", told[4]) };
        assert!(first[0][0] < 80 && first[0][1] == 0, "{first:?}");
        light.act(Action::AuraWhite { kelvin: 2700, level: 25. }, NO_GAP, 0.05);
        light.act(Action::Aura(vec![[0, 0, 255]]), NO_GAP, 0.01);
        assert_eq!(lamp.told().iter().filter(|told| **told == Told::Power(true)).count(), 1, "switched on more than once");
        light.act(Action::Restore, NO_GAP, 0.05);
        let told = lamp.told();
        assert_eq!(told[told.len() - 2..], [Told::Power(false), Told::Stream(false)]);
        // The last frame before it is dark again.
        let dark = told.iter().rev().find_map(|told| if let Told::Frame(frame) = told { Some(frame.clone()) } else { None }).unwrap();
        assert!(dark.iter().flatten().all(|part| *part < 12), "{dark:?}");
        assert!(!told.iter().any(|told| matches!(told, Told::Level(_) | Told::Colour(_) | Told::White(_))));
        // The next film finds it as it was: off.
        light.act(Action::Remember, NO_GAP, 0.05);
        light.act(Action::Fade { to: 5., kelvin: None }, NO_GAP, 0.05);
        assert_eq!(lamp.told().len(), told.len(), "a lamp that is off again was dimmed");
    }

    /// The blink of a lamp that is off switches it on, and off again at
    /// the end with the brightness it kept.
    #[test]
    fn a_blink_puts_an_off_lamp_off_again() {
        let (lamp, light) = Mock::off();
        light.identify();
        let told = lamp.told();
        assert_eq!(told[..2], [Told::Asked, Told::Power(true)]);
        assert_eq!(told[told.len() - 2..], [Told::Level(80), Told::Power(false)]);
        let (lamp, light) = Mock::light(true, 4);
        light.identify();
        assert!(!lamp.told().iter().any(|told| matches!(told, Told::Power(_))), "a lamp that was on was switched");
        assert_eq!(lamp.told().last(), Some(&Told::Level(80)));
    }

    /// The way back to a low light has two commands: the lights are busy
    /// until the second is out, and the program waits for that before it
    /// ends.
    #[test]
    fn a_restore_is_done_before_the_program_ends() {
        let (lamp, light) = Mock::light(true, 14);
        light.act(Action::Remember, NO_GAP, 0.05);
        // Up to the light of a pause, from where the way back goes down.
        light.act(Action::Fade { to: 100., kelvin: None }, NO_GAP, 0.05);
        let lights = Lights(vec![light]);
        lights.act(&Action::Restore, Duration::from_millis(300), 0.05);
        assert_eq!(lamp.levels(), [100, 90], "the first command of the way back");
        assert!(lights.0[0].busy());
        let started = Instant::now();
        lights.settle(Duration::from_secs(5));
        assert!(started.elapsed() >= Duration::from_millis(200), "it did not wait");
        assert_eq!(lamp.levels(), [100, 90, 80], "the light is not back");
        assert!(!lights.0[0].busy());
    }

    /// Each chosen light does what the rules ask.
    #[test]
    fn every_chosen_light_is_told() {
        let (first, a) = Mock::light(true, 14);
        let (second, b) = Mock::light(true, 14);
        let lights = Lights(vec![a, b]);
        lights.act(&Action::Remember, NO_GAP, 0.05);
        lights.act(&Action::Fade { to: 90., kelvin: None }, NO_GAP, 0.05);
        assert_eq!(first.told(), [Told::Asked, Told::Level(90)]);
        assert_eq!(second.told(), first.told());
        assert!(!lights.streams());
        // A pale colour is made stronger and keeps its hue; black is a dim light.
        let [r, g, b] = light_colour([200, 150, 150]);
        assert!(r > 2. * g && g == b, "{r} {g} {b}");
        assert_eq!(light_colour([0, 0, 0]), [10.; 3]);
    }
}
